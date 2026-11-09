//! ok14: the Release 1 contract inventory (D-TXFHR: the contract surfaces; D-E30RA: Release 1
//! ships after M3a; D-XTWM9; D-69AY3). One machine-readable fixture holds thirteen rows, one for
//! each surface the decision lists. The released baseline, `fixtures/contracts/release-1.json`,
//! is frozen at the Release 1 tag; the current fixture, `fixtures/contracts/repo-links-v1.json`,
//! is that baseline with the additions since, and every live comparison and capture uses it. Every row is generated from the code that owns the surface (its registry, parser, schema
//! or an existing fixture), and the tests compare the live generation with the fixture. The members
//! that [`DESCRIPTIVE`] lists are the exception: they describe a layout or a rule in words for the
//! reader, are typed here, and cannot fail on their own; the digests, sizes and probes of their
//! rows are what pin those surfaces. A public contract change after Release 1 must update a versioned fixture and satisfy
//! the upgrade or deprecation rule of D-TXFHR; this file is the baseline of the release candidate.
//!
//! - The `cli` row has two members. `cli.tree` is the real clap command tree: the test-only module
//!   `contract` in rdy-cli walks it and compares it (`ok14_cli_contract_from_clap_is_complete`).
//!   `cli.runtime` is what the release binary does: exit values and JSON field sets, generated
//!   here by running it.
//! - Objects have sorted keys. Where the declared order is the contract (an encoding, a header, a
//!   command's arguments) the value is an array in that order. A set-valued array is sorted.
//! - Absolute temporary paths, random ports and elapsed times are replaced by `<path>`, `<port>`
//!   and `<elapsed>`. No producer commit is in the output.
//! - A listed surface with no source fails the capture; a row cannot be marked covered without an
//!   assertion in its generator.
//!
//! To write a candidate baseline, run the test with `RDY_CONTRACT_CAPTURE=1`: it replaces every
//! row but `cli.tree` (rdy-cli writes that member the same way) and then compares as usual.
use rdy_engine::RdyError;
use rdy_engine::instructions::{Caller, Contribution, LoadRequest, PersonHost, deliver};
use rdy_engine::instructions_config::{PersonPath, PersonSource};
use rdy_engine::tensions::{Involved, SideRef, tension_id, tension_label, tension_vkey};
use rdy_engine::{Scope, config_levels};
use rdy_model::semantic::{PairItem, pair_key};
use rdy_model::{
    ActorRun, Answer, AnswerAct, Assurance, BlockId, Hash, Provenance, RepoId, ReviewSubject, Role,
    Verdict,
    signature::{StatementKind, StatementSource, statement},
};
use rdy_qa::fixtures::{output_within, project};
use rdy_qa::skip::{self, Why};
use rdy_schema::{PackSet, TensionKind, TensionSeverity};
use rdy_store::format::{
    FeatureClass, MANIFEST_VERSION, MAX_FEATURES, MAX_MANIFEST_BYTES, REF_STORE_FEATURES,
    REF_UPGRADE, REGISTERED_FEATURES, baseline_features, registered_features, required_features,
};
use rdy_store::{codec, dump, kinds, sync_stream};
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The thirteen rows of the inventory, sorted: the surfaces of D-TXFHR. The tests pin this list.
const ROWS: [&str; 13] = [
    "cli",
    "config",
    "derived_identities",
    "dump_load",
    "errors_warnings",
    "markdown_exports",
    "mcp",
    "schema_packs",
    "signatures",
    "site_urls",
    "store",
    "sync",
    "working_copy",
];

/// The row members typed by hand: words that describe a surface for the reader. Each is pinned
/// by another member of its row (a fixture digest, a size, a probe), never by itself.
const DESCRIPTIVE: [&str; 12] = [
    "cli.runtime.exit_values",
    "derived_identities.domains",
    "site_urls.block_route",
    "store.encoding_versions.frame_header_bytes",
    "store.feature_rules",
    "store.manifest.name_grammar",
    "store.manifest.required_rdy_grammar",
    "sync.header_fields",
    "sync.logical_keys",
    "sync.order",
    "sync.prerequisite_order",
    "sync.trailer_fields",
];

/// The grammar of the inventory file itself.
const INVENTORY_VERSION: u64 = 1;

fn qa_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn workspace_root() -> PathBuf {
    qa_root()
        .join("../..")
        .canonicalize()
        .expect("the workspace root exists")
}

/// The current contract fixture: the released baseline (`release-1.json`, frozen) with the
/// additions since. A capture writes this file, never the baseline.
fn fixture_path() -> PathBuf {
    qa_root().join("fixtures/contracts/repo-links-v1.json")
}

fn capture_mode() -> bool {
    std::env::var_os("RDY_CONTRACT_CAPTURE").is_some()
}

fn blake3_of(bytes: &[u8]) -> String {
    Hash::of(bytes).to_string()
}

/// The length and blake3 of each file under `dir` (sub-directories walked), by relative path.
fn digests(dir: &Path, keep: impl Fn(&str) -> bool) -> Value {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).expect("read the fixture directory") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                walk(&p, base, out);
            } else {
                out.push(
                    p.strip_prefix(base)
                        .expect("under the base")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut names = vec![];
    walk(dir, dir, &mut names);
    names.sort();
    let mut m = Map::new();
    for n in names.into_iter().filter(|n| keep(n)) {
        let bytes = std::fs::read(dir.join(&n)).expect("read a fixture file");
        m.insert(n, json!(format!("{} {}", bytes.len(), blake3_of(&bytes))));
    }
    assert!(!m.is_empty(), "no fixture file under {}", dir.display());
    Value::Object(m)
}

/// The first byte of the postcard encoding of `v`: its variant index for an enum.
fn ordinal<T: Serialize>(v: &T) -> u64 {
    u64::from(postcard::to_allocvec(v).expect("postcard encodes")[0])
}

/// The shape of a JSON value: objects as their sorted members, an array as the merge of the
/// shapes of all its elements (a member that only some elements hold ends in `?`), scalars as
/// their type. No id, time or text of the run is in it.
fn shape(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            // a member named by an id or a hash is one entry, whatever its id
            let id = Regex::new(r"^([A-Za-z]{1,3}-[0-9A-Za-z]{5,16}|[0-9a-f]{64})$").unwrap();
            let mut out = Map::new();
            for (k, v) in m {
                let key = if id.is_match(k) {
                    "<id>".to_string()
                } else {
                    k.clone()
                };
                let one = shape(v);
                let merged = match out.get(&key) {
                    Some(old) => merge_shape(old, &one),
                    None => one,
                };
                out.insert(key, merged);
            }
            Value::Object(out)
        }
        Value::Array(a) => match a.iter().map(shape).reduce(|x, y| merge_shape(&x, &y)) {
            Some(all) => json!([all]),
            None => json!([]),
        },
        Value::String(_) => json!("string"),
        Value::Number(_) => json!("number"),
        Value::Bool(_) => json!("bool"),
        Value::Null => json!("null"),
    }
}

/// The union of two shapes. An empty array adds nothing. An object member that one side lacks,
/// or marks with `?`, ends in `?`. Two scalar types join with `|` in sorted order. A null with a
/// value becomes `{"|null": value}` and two kinds that differ become `{"|any": [..]}`.
fn merge_shape(a: &Value, b: &Value) -> Value {
    if a == b {
        return a.clone();
    }
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => match (x.first(), y.first()) {
            (Some(x), Some(y)) => json!([merge_shape(x, y)]),
            (Some(x), None) => json!([x]),
            (None, Some(y)) => json!([y]),
            (None, None) => json!([]),
        },
        (Value::Object(x), Value::Object(y))
            if !x.contains_key("|null")
                && !y.contains_key("|null")
                && !x.contains_key("|any")
                && !y.contains_key("|any") =>
        {
            let key = |k: &str| k.trim_end_matches('?').to_string();
            let mut names: Vec<String> = x.keys().chain(y.keys()).map(|k| key(k)).collect();
            names.sort();
            names.dedup();
            let find = |m: &Map<String, Value>, name: &str| {
                m.iter()
                    .find(|(k, _)| key(k) == name)
                    .map(|(k, v)| (k.ends_with('?'), v.clone()))
            };
            let mut out = Map::new();
            for name in names {
                let (fx, fy) = (find(x, &name), find(y, &name));
                let optional = fx.as_ref().is_none_or(|f| f.0) || fy.as_ref().is_none_or(|f| f.0);
                let value = match (fx, fy) {
                    (Some((_, a)), Some((_, b))) => merge_shape(&a, &b),
                    (Some((_, a)), None) => a,
                    (None, Some((_, b))) => b,
                    (None, None) => unreachable!("the name comes from one of the objects"),
                };
                out.insert(if optional { format!("{name}?") } else { name }, value);
            }
            Value::Object(out)
        }
        (Value::String(x), Value::String(y)) => {
            let mut kinds: Vec<&str> = x.split('|').chain(y.split('|')).collect();
            kinds.sort();
            kinds.dedup();
            json!(kinds.join("|"))
        }
        (Value::Object(x), other) | (other, Value::Object(x)) if x.contains_key("|null") => {
            let inner = &x["|null"];
            if other == &json!("null") {
                Value::Object(x.clone())
            } else {
                json!({"|null": merge_shape(inner, other)})
            }
        }
        (Value::String(s), other) | (other, Value::String(s)) if s == "null" => {
            json!({"|null": other})
        }
        (x, y) => {
            let mut both = vec![x.clone(), y.clone()];
            both.sort_by_key(Value::to_string);
            json!({"|any": both})
        }
    }
}

/// Replaces what varies between runs: absolute temporary paths and random ports. Ids, hashes
/// and dates are fixed by the seed helpers, so they stay in the text and are pinned with it.
fn normalize(text: &str, paths: &[&Path]) -> String {
    let mut out = text.to_string();
    // a path as it stands in JSON text too: on Windows its backslashes are doubled there
    let mut replace = |s: &str| {
        out = out.replace(s, "<path>");
        out = out.replace(&s.replace('\\', "\\\\"), "<path>");
    };
    for p in paths {
        // the canonical form first: on Windows it holds the plain one (`\\?\C:\...`)
        if let Ok(c) = p.canonicalize() {
            replace(c.to_string_lossy().as_ref());
        }
        replace(p.to_string_lossy().as_ref());
    }
    // a file below the path keeps one separator, as the fixture has it
    out = out
        .replace("<path>\\\\", "<path>/")
        .replace("<path>\\", "<path>/");
    let out = slash_tails(&out);
    let port = Regex::new(r"127\.0\.0\.1:[0-9]+").unwrap();
    port.replace_all(&out, "127.0.0.1:<port>").into_owned()
}

/// A file deeper below a normalized path keeps its native separators on Windows
/// (`<path>/rdy\config.yaml`, or `<path>/rdy\\config.yaml` in JSON text). The fixture spells
/// every separator `/`, as on Unix. In JSON text only a doubled backslash is a separator, so a
/// string escape such as `\n` after a path stays as it is.
fn slash_tails(text: &str) -> String {
    let (re, sep) = if serde_json::from_str::<Value>(text).is_ok() {
        (r"<path>/[A-Za-z0-9._-]+(?:\\\\[A-Za-z0-9._-]+)+", "\\\\")
    } else {
        (r"<path>/[A-Za-z0-9._-]+(?:\\[A-Za-z0-9._-]+)+", "\\")
    };
    Regex::new(re)
        .unwrap()
        .replace_all(text, |c: &regex::Captures| c[0].replace(sep, "/"))
        .into_owned()
}

#[test]
fn ok14_normalize_spells_windows_file_tails_with_slashes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_string_lossy().to_string();
    // plain text, as `rdy config --show-origin` prints it on Windows
    let text = format!("actor = human:qa  # {root}\\rdy\\config.yaml  # set at: local, user\n");
    assert_eq!(
        normalize(&text, &[dir.path()]),
        "actor = human:qa  # <path>/rdy/config.yaml  # set at: local, user\n"
    );
    // JSON text: the separators are doubled, and an escape after the path is kept
    let json_root = root.replace('\\', "\\\\");
    let text = format!(
        "{{\"file\": \"{json_root}\\\\.rdy\\\\local.yaml\", \"m\": \"in {json_root}\\\\a.yaml\\nnext\"}}"
    );
    assert_eq!(
        normalize(&text, &[dir.path()]),
        "{\"file\": \"<path>/.rdy/local.yaml\", \"m\": \"in <path>/a.yaml\\nnext\"}"
    );
    // a Unix tail is unchanged
    let text = format!("# {root}/.rdy/local.yaml\n");
    assert_eq!(
        normalize(&text, &[dir.path()]),
        "# <path>/.rdy/local.yaml\n"
    );
}

// ---- source scanning ----

/// The end of a comment, string or character literal that starts at `i`, or `None` when `i` is
/// code. Raw strings, byte strings and lifetimes are told apart from character literals.
fn skip_literal(b: &[u8], i: usize) -> Option<usize> {
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let prev = |k: usize| if i >= k { Some(b[i - k]) } else { None };
    if b[i..].starts_with(b"//") {
        return Some(
            b[i..]
                .iter()
                .position(|c| *c == b'\n')
                .map_or(b.len(), |p| i + p),
        );
    }
    if b[i..].starts_with(b"/*") {
        let (mut depth, mut k) = (0usize, i);
        while k < b.len() {
            if b[k..].starts_with(b"/*") {
                depth += 1;
                k += 2;
            } else if b[k..].starts_with(b"*/") {
                depth -= 1;
                k += 2;
                if depth == 0 {
                    return Some(k);
                }
            } else {
                k += 1;
            }
        }
        return Some(b.len());
    }
    let raw_start = b[i] == b'r'
        && match prev(1) {
            None => true,
            Some(c) if !ident(c) => true,
            Some(b'b') => !prev(2).is_some_and(ident),
            Some(_) => false,
        };
    if raw_start {
        let mut k = i + 1;
        while k < b.len() && b[k] == b'#' {
            k += 1;
        }
        if k < b.len() && b[k] == b'"' {
            let hashes = k - i - 1;
            let mut close = vec![b'"'];
            close.extend(std::iter::repeat_n(b'#', hashes));
            let from = k + 1;
            let end = b[from..]
                .windows(close.len())
                .position(|w| w == close.as_slice())
                .map_or(b.len(), |p| from + p + close.len());
            return Some(end);
        }
        return None;
    }
    if b[i] == b'"' {
        let mut k = i + 1;
        while k < b.len() {
            match b[k] {
                b'\\' => k += 2,
                b'"' => return Some(k + 1),
                _ => k += 1,
            }
        }
        return Some(b.len());
    }
    if b[i] == b'\'' {
        if i + 1 < b.len() && b[i + 1] == b'\\' {
            // an escape: the closing quote is the first one after the escaped character
            let end = b[(i + 3).min(b.len())..]
                .iter()
                .position(|c| *c == b'\'')
                .map_or(b.len(), |p| i + 3 + p + 1);
            return Some(end);
        }
        // one character, then the closing quote; anything else is a lifetime
        let width = match b.get(i + 1) {
            Some(c) if *c >= 0xF0 => 4,
            Some(c) if *c >= 0xE0 => 3,
            Some(c) if *c >= 0xC0 => 2,
            _ => 1,
        };
        if b.get(i + 1 + width) == Some(&b'\'') {
            return Some(i + 2 + width);
        }
        return Some(i + 1);
    }
    None
}

/// The first byte after the whitespace, comments and attributes that start at `i`.
fn skip_trivia_and_attrs(b: &[u8], mut i: usize) -> usize {
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            return i;
        }
        if b[i..].starts_with(b"//") || b[i..].starts_with(b"/*") {
            i = skip_literal(b, i).unwrap_or(i + 1);
        } else if b[i..].starts_with(b"#[") || b[i..].starts_with(b"#![") {
            let (mut depth, mut k) = (0usize, i);
            while k < b.len() {
                if let Some(n) = skip_literal(b, k) {
                    k = n;
                    continue;
                }
                match b[k] {
                    b'[' => depth += 1,
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            k += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
            i = k;
        } else {
            return i;
        }
    }
}

/// The end of the item (or field, or statement, or match arm) that starts at `i`: just past its
/// closing `;`, `,` or `}`, or at the closer of the enclosing block when the item ends without one.
fn item_end(b: &[u8], i: usize) -> usize {
    #[derive(PartialEq)]
    enum Mode {
        Item,
        Let,
        /// `if`, `match`, `while`, `for`, `loop`: ends at its closing brace, past any `else` chain.
        Control,
        Other,
    }
    let start = skip_trivia_and_attrs(b, i);
    // the first words decide how the item ends
    let words: Vec<String> = {
        let mut ws = vec![];
        let mut k = start;
        while ws.len() < 4 && k < b.len() {
            while k < b.len() && !(b[k].is_ascii_alphanumeric() || b[k] == b'_') {
                if matches!(b[k], b'{' | b'}' | b';' | b',' | b':' | b'=') {
                    k = b.len();
                    break;
                }
                k += 1;
            }
            let from = k;
            while k < b.len() && (b[k].is_ascii_alphanumeric() || b[k] == b'_') {
                k += 1;
            }
            if from < k {
                ws.push(String::from_utf8_lossy(&b[from..k]).into_owned());
            }
        }
        ws
    };
    let item_words = [
        "fn",
        "struct",
        "enum",
        "impl",
        "mod",
        "trait",
        "use",
        "static",
        "const",
        "type",
        "extern",
        "unsafe",
        "async",
        "macro_rules",
        "union",
    ];
    let first = words
        .iter()
        .find(|w| {
            w.as_str() != "pub"
                && w.as_str() != "crate"
                && w.as_str() != "super"
                && w.as_str() != "in"
        })
        .map(String::as_str)
        .unwrap_or("");
    // `name! { .. }`: a macro invoked with braces ends at its closing brace, like an item
    let braced_macro = {
        let mut k = start;
        while k < b.len() && (b[k].is_ascii_alphanumeric() || b[k] == b'_' || b[k] == b':') {
            k += 1;
        }
        let bang = k > start && b.get(k) == Some(&b'!');
        let mut m = k + 1;
        while bang && m < b.len() && b[m].is_ascii_whitespace() {
            m += 1;
        }
        bang && b.get(m) == Some(&b'{')
    };
    let mode = if first == "let" {
        Mode::Let
    } else if item_words.contains(&first) || braced_macro {
        Mode::Item
    } else if ["if", "match", "while", "for", "loop"].contains(&first) {
        Mode::Control
    } else {
        Mode::Other
    };
    let starts_with_block = b.get(start) == Some(&b'{');
    let (mut paren, mut brace, mut angle) = (0i64, 0i64, 0i64);
    let (mut opened, mut arrow) = (false, false);
    let mut k = start;
    while k < b.len() {
        if let Some(n) = skip_literal(b, k) {
            k = n;
            continue;
        }
        let c = b[k];
        match c {
            b'(' | b'[' => paren += 1,
            b')' | b']' => {
                paren -= 1;
                if paren < 0 {
                    return k;
                }
            }
            b'{' => {
                brace += 1;
                if brace == 1 && paren == 0 {
                    opened = true;
                }
            }
            b'}' => {
                brace -= 1;
                if brace < 0 {
                    return k;
                }
                if brace == 0 && paren == 0 && opened {
                    let block_ends = match mode {
                        Mode::Item => true,
                        Mode::Let => false,
                        Mode::Other => starts_with_block || arrow,
                        Mode::Control => {
                            // the statement goes on after `else` and in a method call or `?`
                            let mut m = k + 1;
                            while m < b.len() && b[m].is_ascii_whitespace() {
                                m += 1;
                            }
                            let else_follows = b[m..].starts_with(b"else")
                                && !b
                                    .get(m + 4)
                                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_');
                            !(else_follows || matches!(b.get(m), Some(b'.') | Some(b'?')))
                        }
                    };
                    if block_ends {
                        return k + 1;
                    }
                }
            }
            b'=' if b.get(k + 1) == Some(&b'>') => {
                arrow = true;
                k += 1;
            }
            b'-' if b.get(k + 1) == Some(&b'>') => k += 1,
            b'<' if paren == 0 && brace == 0 => angle += 1,
            b'>' if paren == 0 && brace == 0 => angle -= 1,
            b';' if paren == 0 && brace == 0 => return k + 1,
            b',' if paren == 0 && brace == 0 && angle <= 0 && mode == Mode::Other => return k + 1,
            _ => {}
        }
        k += 1;
    }
    b.len()
}

/// The source text without its test-only items: each `#[cfg(test)]` and the one item it marks
/// (a module to its closing brace, a function, field, `use` or statement to its end) are blanked,
/// newlines kept so a line number still points into the original. Everything else stays, and the
/// scan never stops at the first attribute.
fn product_text(src: &str) -> String {
    let mut out = src.as_bytes().to_vec();
    for (from, to) in test_regions(src) {
        for byte in &mut out[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(out).expect("blanking keeps the text valid")
}

/// The byte ranges `product_text` blanks: each `#[cfg(test)]` with the one item it marks (a
/// whole file for `#![cfg(test)]`).
fn test_regions(src: &str) -> Vec<(usize, usize)> {
    let b = src.as_bytes();
    if b.starts_with(b"#![cfg(test)]") {
        return vec![(0, b.len())];
    }
    let mut regions = vec![];
    let mut i = 0;
    while i < b.len() {
        if let Some(n) = skip_literal(b, i) {
            i = n;
            continue;
        }
        if b[i..].starts_with(b"#[cfg(test)]") {
            let end = item_end(b, i + "#[cfg(test)]".len()).max(i + "#[cfg(test)]".len());
            regions.push((i, end));
            i = end;
            continue;
        }
        i += 1;
    }
    regions
}

/// The line, without a trailing `//` comment that sits outside a string.
fn code_of(line: &str) -> &str {
    let mut quotes = 0;
    for (n, c) in line.char_indices() {
        if c == '"' {
            quotes += 1;
        } else if c == '/' && quotes % 2 == 0 && line[n..].starts_with("//") {
            return &line[..n];
        }
    }
    line
}

/// The byte ranges of the top-level, comma-separated parts of `line` (commas inside brackets do
/// not split): a match arm is one part, so two arms on one line are two parts.
fn parts_of(line: &str) -> Vec<(usize, usize)> {
    let b = line.as_bytes();
    let (mut parts, mut depth, mut from, mut k) = (vec![], 0i64, 0usize, 0usize);
    while k < b.len() {
        if let Some(n) = skip_literal(b, k) {
            k = n;
            continue;
        }
        match b[k] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b',' if depth == 0 => {
                parts.push((from, k));
                from = k + 1;
            }
            _ => {}
        }
        k += 1;
    }
    parts.push((from, b.len()));
    parts
}

/// Whether the text that ends at a needle (`before`) leaves it inside the parentheses of a
/// `matches!(`: the last one opened and not yet closed. A `matches!(..)` that closed before the
/// needle (`if matches!(a, B) { Verdict::Pass }`) does not read it.
fn inside_matches(before: &str) -> bool {
    let Some(open) = before.rfind("matches!(") else {
        return false;
    };
    let mut depth = 0i32;
    for c in before[open + "matches!".len()..].chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return false;
        }
    }
    depth > 0
}

/// The places in `product` (already free of test items) that construct a carried value. A
/// comparison, a `matches!` and the pattern side of a match arm read it. A carried value to the
/// right of `=>`, or anywhere else, writes it.
fn carried_writes(rel: &str, product: &str, needles: &[&str]) -> Vec<String> {
    let mut hits = vec![];
    for (n, raw) in product.lines().enumerate() {
        let line = code_of(raw);
        if line.trim_start().starts_with("//") {
            continue;
        }
        for needle in needles {
            for (pos, _) in line.match_indices(needle) {
                let (from, to) = parts_of(line)
                    .into_iter()
                    .find(|(f, t)| *f <= pos && pos < *t)
                    .unwrap_or((0, line.len()));
                let part = &line[from..to];
                let (before, after) = (&part[..pos - from], &part[pos - from + needle.len()..]);
                let in_pattern = part
                    .find("=>")
                    .is_some_and(|a| a >= pos - from + needle.len());
                let compared = before.trim_end().ends_with("==")
                    || before.trim_end().ends_with("!=")
                    || after.trim_start().starts_with("==")
                    || after.trim_start().starts_with("!=")
                    || inside_matches(before);
                if !in_pattern && !compared {
                    hits.push(format!("{rel}:{}: {}", n + 1, raw.trim()));
                }
            }
        }
    }
    hits
}

// ---- store ----

fn store_row() -> Value {
    // the activation rule: what a first write of each kind and encoding turns on
    let mut activation = Map::new();
    for kind in dump::current_frame_kinds() {
        for version in [0u16, 1] {
            let (raw, payload) = if version == 0 {
                (*kind, vec![])
            } else {
                (*kind | codec::VERSIONED, 1u16.to_le_bytes().to_vec())
            };
            let names: Vec<String> = required_features(raw, &payload)
                .into_iter()
                .map(|f| f.name)
                .collect();
            if !names.is_empty() {
                activation.insert(format!("kind {kind} encoding {version}"), json!(names));
            }
        }
    }
    assert!(
        activation.len() >= 5,
        "the activation rules of the carried kinds have a source"
    );
    let kind_ids: BTreeMap<String, u8> = [
        ("BLOCK", kinds::BLOCK),
        ("CHANGE", kinds::CHANGE),
        ("PROPOSAL", kinds::PROPOSAL),
        ("LANDING", kinds::LANDING),
        ("SNAPSHOT", kinds::SNAPSHOT),
        ("BLOB_CHUNK", kinds::BLOB_CHUNK),
        ("BLOB_MANIFEST", kinds::BLOB_MANIFEST),
        ("REF_UPDATE", kinds::REF_UPDATE),
        ("TAG", kinds::TAG),
        ("ANSWER", kinds::ANSWER),
        ("SIGNATURE", kinds::SIGNATURE),
        ("VIEW_OP", kinds::VIEW_OP),
        ("BATCH_COMMIT", kinds::BATCH_COMMIT),
        ("INTENT", kinds::INTENT),
        ("ALT_SET", kinds::ALT_SET),
        ("REVIEW", kinds::REVIEW),
        ("TENSION", kinds::TENSION),
        ("TRANSITION", kinds::TRANSITION),
        ("REDACTION", kinds::REDACTION),
        ("REDACTION_RESIDUE", kinds::REDACTION_RESIDUE),
        ("REDACTION_APPLY", kinds::REDACTION_APPLY),
        ("REDACTION_CARRIED", kinds::REDACTION_CARRIED),
        ("PLAN", kinds::PLAN),
        ("ASSESSMENT", kinds::ASSESSMENT),
        ("DISPUTE", kinds::DISPUTE),
        ("DISPUTE_RESOLUTION", kinds::DISPUTE_RESOLUTION),
        ("CHECK_REQUEST", kinds::CHECK_REQUEST),
        ("FOREIGN", kinds::FOREIGN),
    ]
    .into_iter()
    .map(|(n, k)| (n.to_string(), k))
    .collect();
    assert_eq!(
        kind_ids.values().max(),
        Some(&kinds::FOREIGN),
        "the kind ids end at FOREIGN"
    );
    let latest: BTreeMap<String, u16> = kind_ids
        .iter()
        .map(|(n, k)| (n.clone(), codec::latest(*k)))
        .collect();
    let first: BTreeMap<String, u16> = kind_ids
        .iter()
        .map(|(n, k)| (n.clone(), codec::first(*k)))
        .collect();
    let request = Hash([0; 32]);
    let subject = |s: ReviewSubject| ordinal(&s);
    let review_subjects = json!({
        "Tension": subject(ReviewSubject::Tension {
            id: request,
            vkey: request,
            kind: String::new(),
            severity: String::new(),
            sides: vec![],
            blocks: vec![],
        }),
        "Mention": subject(ReviewSubject::Mention {
            block: BlockId::parse("c-AAAAA").unwrap(),
            entity: BlockId::parse("c-AAAAA").unwrap(),
        }),
        "Pair": subject(ReviewSubject::Pair { key: request, items: vec![] }),
        "Check": subject(ReviewSubject::Check { request }),
    });
    assert_eq!(review_subjects["Check"], 3);
    let verdicts = json!({
        "Dismiss": ordinal(&Verdict::Dismiss),
        "Restore": ordinal(&Verdict::Restore),
        "Tension": ordinal(&Verdict::Tension),
        "NoTension": ordinal(&Verdict::NoTension),
        "Pass": ordinal(&Verdict::Pass),
        "Concern": ordinal(&Verdict::Concern),
        "Fail": ordinal(&Verdict::Fail),
    });
    assert_eq!(
        (
            verdicts["Pass"].clone(),
            verdicts["Concern"].clone(),
            verdicts["Fail"].clone()
        ),
        (json!(4), json!(5), json!(6))
    );
    let acts: Vec<(&str, AnswerAct)> = vec![
        ("RejectProposal", AnswerAct::RejectProposal),
        ("Later", AnswerAct::Later),
        ("Confirm", AnswerAct::Confirm),
        ("RejectDecision", AnswerAct::RejectDecision),
        ("AnswerQuestion", AnswerAct::AnswerQuestion),
        ("Dismiss", AnswerAct::Dismiss),
        ("Tag", AnswerAct::Tag),
        ("ViewOwn", AnswerAct::ViewOwn),
        ("CloseStale", AnswerAct::CloseStale),
        ("Propose", AnswerAct::Propose),
        ("KeepSuspect", AnswerAct::KeepSuspect),
        ("AcceptDeferrals", AnswerAct::AcceptDeferrals),
        ("RefuseDeferrals", AnswerAct::RefuseDeferrals),
        ("RejectReaffirm", AnswerAct::RejectReaffirm),
        ("Dispute", AnswerAct::Dispute),
        ("ResolveDispute", AnswerAct::ResolveDispute),
        ("AcceptVerdict", AnswerAct::AcceptVerdict),
        ("Adopt", AnswerAct::Adopt),
        ("KeepPin", AnswerAct::KeepPin),
    ];
    let answer_acts: Map<String, Value> = acts
        .iter()
        .map(|(n, a)| ((*n).to_string(), json!(ordinal(a))))
        .collect();
    assert_eq!(
        (
            answer_acts["Dispute"].clone(),
            answer_acts["ResolveDispute"].clone(),
            answer_acts["AcceptVerdict"].clone()
        ),
        (json!(14), json!(15), json!(16))
    );
    for (i, (name, _)) in acts.iter().enumerate() {
        assert_eq!(
            answer_acts[*name],
            json!(i as u64),
            "ordinals are declaration order: {name}"
        );
    }
    use rdy_model::plan::{PlanAct, PlanKind, PlanOutcome};
    let other_ordinals = json!({
        "Assurance": {
            "Policy": ordinal(&Assurance::Policy),
            "Relayed": ordinal(&Assurance::Relayed),
            "Local": ordinal(&Assurance::Local),
        },
        "PlanAct": {
            "Confirm": ordinal(&PlanAct::Confirm), "Defer": ordinal(&PlanAct::Defer),
            "Done": ordinal(&PlanAct::Done), "Dismiss": ordinal(&PlanAct::Dismiss),
        },
        "PlanKind": {
            "Update": ordinal(&PlanKind::Update), "Rejustify": ordinal(&PlanKind::Rejustify),
            "Reaffirm": ordinal(&PlanKind::Reaffirm), "Confirm": ordinal(&PlanKind::Confirm),
            "UpdateImplementation": ordinal(&PlanKind::UpdateImplementation),
            "Tension": ordinal(&PlanKind::Tension),
        },
        "PlanOutcome": {
            "Updated": ordinal(&PlanOutcome::Updated), "Relinked": ordinal(&PlanOutcome::Relinked),
            "Repinned": ordinal(&PlanOutcome::Repinned), "Reaffirmed": ordinal(&PlanOutcome::Reaffirmed),
            "Retired": ordinal(&PlanOutcome::Retired), "Committed": ordinal(&PlanOutcome::Committed),
            "Confirmed": ordinal(&PlanOutcome::Confirmed), "Done": ordinal(&PlanOutcome::Done),
            "Deferred": ordinal(&PlanOutcome::Deferred), "Open": ordinal(&PlanOutcome::Open),
            "Dismissed": ordinal(&PlanOutcome::Dismissed),
        },
    });
    // the stable object ids: the identity keys (kind and hash of every stored frame) that the
    // released fixtures pin, and the content domain every block hash derives from
    let fixtures = qa_root().join("fixtures");
    let mut object_ids = Map::new();
    for set in [
        "v3-s18.expect.json",
        "v4-f1-e0.expect.json",
        "v4-f1-e1.expect.json",
    ] {
        let text = std::fs::read_to_string(fixtures.join("dump").join(set)).expect("expect file");
        let v: Value = serde_json::from_str(&text).expect("expect JSON");
        let keys = &v["whole"]["store"]["identity"]["keys"];
        let list: Vec<&str> = keys
            .as_array()
            .unwrap_or_else(|| panic!("{set} holds no identity keys: {keys}"))
            .iter()
            .map(|k| k.as_str().expect("key text"))
            .collect();
        let mut by_kind: BTreeMap<String, u64> = BTreeMap::new();
        for k in &list {
            *by_kind
                .entry(k.split(':').next().unwrap().to_string())
                .or_default() += 1;
        }
        object_ids.insert(
            set.trim_end_matches(".expect.json").to_string(),
            json!({
                "objects": list.len(),
                "by_kind": by_kind,
                "keys_blake3": blake3_of(list.join("\n").as_bytes()),
            }),
        );
    }
    json!({
        "sources": [
            "rdy_store::format", "rdy_store::codec", "rdy_store::kinds", "rdy_model enums",
            "fixtures/store/feature-1", "fixtures/dump/*.expect.json",
        ],
        "manifest": {
            "version": MANIFEST_VERSION,
            "max_bytes": MAX_MANIFEST_BYTES,
            "max_features": MAX_FEATURES,
            "format_file": rdy_store::format::FORMAT_FILE,
            "format_text": rdy_store::format::format_file_text(),
            "features_file": rdy_store::format::FEATURES_FILE,
            "features_first_line": baseline_features()
                .to_text()
                .lines()
                .next()
                .expect("FEATURES has a first line"),
            "authority_ref": REF_STORE_FEATURES,
            "upgrade_receipt_ref": REF_UPGRADE,
            "name_grammar": "[a-z][a-z0-9_]{0,63}",
            "required_rdy_grammar": "three decimal u32 components joined by dots",
        },
        "classes": [
            FeatureClass::Compatible.as_str(),
            FeatureClass::ReadOnlyCompatible.as_str(),
            FeatureClass::Incompatible.as_str(),
        ],
        "features": registered_features(),
        "registered_names": REGISTERED_FEATURES,
        "creation_baseline": baseline_features(),
        "creation_baseline_text": baseline_features().to_text(),
        "activation": activation,
        "feature_rules": {
            "assessment": "an ASSESSMENT, or a LANDING whose assessment is set",
            "check_request": "a CHECK_REQUEST, a REVIEW with subject Check or verdict Pass, Concern or Fail, or an ANSWER with act AcceptVerdict",
            "content_hash_v2": "baseline: every store this rdy writes",
            "dispute": "a DISPUTE, a DISPUTE_RESOLUTION, or an ANSWER with act Dispute or ResolveDispute",
            "foreign_copy": "a FOREIGN, any frame in its kind's optional encoding, or a repo_link block, alone or in a change",
            "object_encoding": "any frame of encoding 1 or more",
            "plan_tension": "a PLAN with a sealed Tension item or Dismissed outcome, or an answer with act Dismiss",
            "view_replay": "a CHANGE whose links.replayed_from is set",
        },
        "encoding_versions": {
            "frame_header_bytes": 37,
            "versioned_flag": codec::VERSIONED,
            "registered": codec::REGISTERED.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
            "optional": codec::OPTIONAL.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
            "latest": latest,
            "first": first,
        },
        "kind_ids": kind_ids,
        "current_frame_kinds": dump::current_frame_kinds(),
        "ref_families": rdy_qa::every_kind::FAMILIES,
        "ordinals": {
            "AnswerAct": answer_acts,
            "ReviewSubject": review_subjects,
            "Verdict": verdicts,
            "others": other_ordinals,
        },
        "content_domain": String::from_utf8_lossy(rdy_model::block::CONTENT_DOMAIN).trim_end_matches('\0'),
        "object_ids": object_ids,
        "native_fixture": digests(&fixtures.join("store/feature-1"), |_| true),
    })
}

// ---- dump and load ----

/// An MCP client on `dir` whose home and config are the empty home of this test binary: a probe
/// reads no operator file on any platform, and its replies cannot depend on one.
fn isolated_mcp(dir: &Path) -> rdy_qa::mcp::McpClient {
    rdy_qa::mcp::McpClient::start_isolated(dir, rdy_qa::fixtures::empty_home())
}

/// Runs the release binary in `dir` without a daemon; returns its exit value, stdout and stderr.
fn rdy_in(dir: &Path, env: &[(String, String)], args: &[&str]) -> (i32, String, String) {
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.args(["--no-daemon", "--project"])
        .arg(dir)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .current_dir(dir);
    let o = output_within(&mut cmd, Duration::from_secs(120)).expect("run rdy");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn dump_load_row() -> Value {
    let dump_dir = qa_root().join("fixtures/dump");
    let pairs: Vec<Value> = dump::READS
        .iter()
        .map(|(d, s)| {
            json!({
                "dump_format": d,
                "store_format": s,
                "frame_kinds": dump::frame_kinds(*d, *s).expect("a released pair has kinds"),
            })
        })
        .collect();
    assert_eq!(pairs.len(), 9, "nine released pairs");
    let probes: Vec<Value> = [
        ".rdy.yaml",
        ".rdyignore",
        ".rdy/packs/team.yaml",
        ".rdy/packs/.hidden.yaml",
        ".rdy/packs/x.txt",
        "docs/a.yaml",
    ]
    .iter()
    .map(|p| json!([p, dump::is_project_file(p)]))
    .collect();
    // empty-target behaviour: a load into an empty directory, then into the loaded one
    let mut empty_target = Map::new();
    let (cfg, env) = rdy_qa::fixtures::human_env("human:qa");
    let target = tempfile::tempdir().expect("tempdir");
    let source = dump_dir.join("v4-f1-e0.rdydump");
    let (code, out, _) = rdy_in(
        target.path(),
        &env,
        &["--json", "load", source.to_str().unwrap(), "."],
    );
    let loaded: Value = serde_json::from_str(out.trim()).unwrap_or(json!(null));
    empty_target.insert(
        "into_empty_directory".into(),
        json!({"exit": code, "reply": shape(&loaded)}),
    );
    let (code, _, err) = rdy_in(
        target.path(),
        &env,
        &["--json", "load", source.to_str().unwrap(), "."],
    );
    let refused: Value = serde_json::from_str(err.trim()).unwrap_or(json!(null));
    empty_target.insert(
        "into_a_directory_with_a_store".into(),
        json!({
            "exit": code,
            "code": refused["code"],
            "reply": shape(&refused),
        }),
    );
    drop(cfg);
    assert_eq!(
        empty_target["into_empty_directory"]["exit"], 0,
        "the load of a released fixture into an empty directory succeeds: {empty_target:?}"
    );
    json!({
        "sources": ["rdy_store::dump", "fixtures/dump", "fixtures/sync", "rdy load"],
        "magic": String::from_utf8_lossy(dump::MAGIC),
        "dump_format": dump::DUMP_FORMAT,
        "reads_features": dump::READS_FEATURES,
        "released_pairs": pairs,
        "current_frame_kinds": dump::current_frame_kinds(),
        "container_record_kinds": {
            "HEADER": dump::rec::HEADER, "FILE": dump::rec::FILE, "BATCH": dump::rec::BATCH,
            "DROPPED": dump::rec::DROPPED, "TRAILER": dump::rec::TRAILER,
        },
        "sizes": {
            "header_fixed": dump::HEADER_FIXED, "trailer": dump::TRAILER_LEN,
            "dropped": dump::DROPPED_LEN, "encoded_dropped": dump::ENCODED_DROPPED_LEN,
            "max_files": dump::MAX_FILES, "max_file_bytes": dump::MAX_FILE_BYTES,
        },
        "project_file_probes": probes,
        "fixture_digests": digests(&dump_dir, |_| true),
        "empty_target": empty_target,
    })
}

// ---- sync ----

fn sync_row() -> Value {
    let sync_dir = qa_root().join("fixtures/sync");
    let carrier = sync_dir.join("v2-f1-e1.rdysync");
    let report = rdy_engine::stream_check::check_batch(
        std::fs::File::open(&carrier).expect("the carrier fixture opens"),
        None,
    )
    .expect("the carrier fixture is a valid batch");
    let mut kinds_seen: BTreeMap<u8, u64> = BTreeMap::new();
    for k in &report.keys {
        *kinds_seen.entry(k.kind).or_default() += 1;
    }
    let header = serde_json::to_value(&report.header).expect("header JSON");
    json!({
        "sources": ["rdy_store::sync_stream", "fixtures/sync", "rdy_engine::stream_check"],
        "magic": String::from_utf8_lossy(sync_stream::MAGIC),
        "stream_format": sync_stream::STREAM_FORMAT,
        "record_kinds": {
            "HEADER": sync_stream::rec::HEADER,
            "DROPPED": sync_stream::rec::DROPPED,
            "TRAILER": sync_stream::rec::TRAILER,
        },
        "order": ["HEADER", "object frames and DROPPED records", "TRAILER"],
        "header_fields": [
            "features_len: u32", "features: canonical JSON", "repo_id: [u8; 16]",
            "purpose: u8", "tag: [u8; 32]", "prerequisites: u32", "keys: kind u8 + hash [u8; 32]",
        ],
        "purposes": purposes(),
        "logical_keys": "kind: u8 || hash: [u8; 32]",
        "prerequisite_order": "strictly ascending logical keys",
        "trailer_fields": ["records: u64", "bytes: u64", "digest: [u8; 32]"],
        "sizes": {
            "header_fixed": sync_stream::HEADER_FIXED, "key": sync_stream::KEY_LEN,
            "trailer": sync_stream::TRAILER_LEN, "max_header": sync_stream::MAX_HEADER_BYTES,
            "max_prerequisites": sync_stream::MAX_PREREQUISITES,
            "max_stream_bytes": sync_stream::MAX_STREAM_BYTES,
            "max_records": sync_stream::MAX_RECORDS, "max_edges": sync_stream::MAX_EDGES,
            "max_history_depth": sync_stream::MAX_HISTORY_DEPTH,
            "max_batch_files": sync_stream::MAX_BATCH_FILES,
        },
        "sync_kinds": sync_stream::SYNC_KINDS,
        "carrier_fixture": {
            "name": "v2-f1-e1.rdysync",
            "header": header,
            "records": report.records,
            "bytes": report.bytes,
            "digest": report.digest.to_string(),
            "kinds": kinds_seen,
        },
        "fixture_digests": digests(&sync_dir, |_| true),
    })
}

// ---- working copy ----

/// The checked-out project of the working-copy row: a doc in a folder with a claim.
fn wc_project() -> (rdy_qa::daemon::Served, Vec<String>) {
    rdy_model::seed_ids_for_tests(14);
    let _clock = rdy_qa::golden::fixture_clock();
    let s = rdy_qa::daemon::Served::new();
    let mut r = rdy_engine::Rdy::open(s.dir.path()).expect("open the served project");
    let w = rdy_qa::fixtures::write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "guides", "title": "Guides"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "intro", "title": "Intro", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Start"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Hello."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "after": "$p", "body": "Claims hold.", "links": {"describes": [{"target": "$p"}]}}
        ]),
    )
    .expect("seed the tree");
    rdy_qa::fixtures::land(&mut r, "work").expect("land the seed");
    r.close().expect("close");
    rdy_model::unseed_ids_for_tests();
    let ids = ["$f", "$d", "$s", "$p", "$c"]
        .iter()
        .map(|k| w.created[*k].clone())
        .collect();
    (s, ids)
}

fn wc_row() -> Value {
    let (s, ids) = wc_project();
    let run = |args: &[&str]| -> (bool, Value, String) {
        let mut a = vec!["--no-daemon", "--json"];
        a.extend_from_slice(args);
        let o = s.rdy(&a);
        let text = String::from_utf8_lossy(&o.stdout).into_owned();
        let err = String::from_utf8_lossy(&o.stderr).into_owned();
        let v = serde_json::from_str(text.trim())
            .or_else(|_| serde_json::from_str(err.trim()))
            .unwrap_or(json!(null));
        (o.status.success(), v, err)
    };
    let (ok, checkout, _) = run(&["checkout"]);
    assert!(ok, "the checkout succeeds: {checkout}");
    // the sidecar state under .rdy/wc, by file, with the shape of each YAML file
    let wc_dir = s.dir.path().join(".rdy/wc");
    let mut state = Map::new();
    let mut files: Vec<String> = vec![];
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).expect("read dir") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                walk(&p, base, out);
            } else {
                out.push(
                    p.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    walk(&wc_dir, &wc_dir, &mut files);
    files.sort();
    let mut state_names: Vec<String> = files
        .iter()
        .map(|f| {
            // a base copy or a stage file is named by the path it holds
            if f.starts_with("base/") || f.starts_with("stage/") {
                format!("{}<path>", &f[..f.find('/').unwrap() + 1])
            } else {
                f.clone()
            }
        })
        .collect();
    state_names.dedup();
    for f in files
        .iter()
        .filter(|f| f.ends_with(".yaml") && !f.starts_with("base/"))
    {
        let text = std::fs::read_to_string(wc_dir.join(f)).expect("state file");
        let v: Value = serde_yaml_ng::from_str(&text).unwrap_or(json!(null));
        state.insert(f.clone(), shape(&v));
    }
    // the member files, as the person edits them
    let mut members = Map::new();
    for rel in ["guides/_folder.yaml", "guides/intro.yaml"] {
        let text = std::fs::read_to_string(s.dir.path().join(rel)).expect("member file");
        let v: Value = serde_yaml_ng::from_str(&text).expect("member YAML");
        members.insert(rel.to_string(), shape(&v));
    }
    // the edit input grammar: an unknown key is refused, with the nearest known key named
    let intro = s.dir.path().join("guides/intro.yaml");
    let original = std::fs::read_to_string(&intro).expect("read the doc file");
    let edited = original.replacen("  body: Claims hold.", "  bodyy: Claims hold.", 1);
    let bad = if edited == original {
        original.replacen("body: Claims hold.", "bodyy: Claims hold.", 1)
    } else {
        edited
    };
    assert_ne!(
        bad, original,
        "the claim's body line is in the file:\n{original}"
    );
    std::fs::write(&intro, &bad).expect("write the edited file");
    let (_ok, refusal, err) = run(&["check"]);
    let refusal = if refusal.is_null() {
        json!({"stderr": normalize(&err, &[s.dir.path()])})
    } else {
        refusal
    };
    let refusal = serde_json::from_str::<Value>(&normalize(&refusal.to_string(), &[s.dir.path()]))
        .unwrap_or(refusal);
    std::fs::write(&intro, &original).expect("restore the doc file");
    let commits = wc_commit_replies(&s, &ids[2]);
    let mut ids_seen = ids;
    ids_seen.sort();
    json!({
        "sources": ["rdy checkout", ".rdy/wc", "rdy check", "rdy commit", "rdy_engine::wc"],
        "state_directory": ".rdy/wc",
        "state_files": state_names,
        "state_shapes": state,
        "member_shapes": members,
        "checkout_reply": shape(&checkout),
        "commit_replies": commits,
        "unknown_key_refusal": refusal,
        "state_constants": {
            "wc_dir": rdy_engine::wc::state::WC_DIR,
            "worktrees_dir": rdy_engine::wc::state::WORKTREES_DIR,
            "pointer_file": rdy_engine::wc::state::POINTER_FILE,
            "checkout_file": rdy_engine::wc::state::CHECKOUT_FILE,
            "update_file": rdy_engine::wc::state::UPDATE_FILE,
            "max_briefs": rdy_engine::wc::state::MAX_BRIEFS,
            "blobs_dir": rdy_engine::wc::layout::BLOBS_DIR,
            "theirs_suffix": rdy_engine::wc::layout::THEIRS_SUFFIX,
            "folder_theirs": rdy_engine::wc::layout::FOLDER_THEIRS,
            "max_member_bytes": rdy_engine::wc::layout::MAX_MEMBER_BYTES,
        },
        "seeded_blocks": ids_seen.len(),
    })
}

/// nb9 (O9, FW-E4): the replies of a commit that creates a prose block under `section`, by the
/// person (no daemon) and by an agent (through the daemon, which an agent commit needs). The
/// person's reply holds the write's receipt as `briefing` and the briefing the CLI made for the
/// commit as `cited_briefing`; the agent's holds the receipt and no cited briefing.
fn wc_commit_replies(s: &rdy_qa::daemon::Served, section: &str) -> Value {
    let intro = s.dir.path().join("guides/intro.yaml");
    let add_prose = |body: &str| {
        let orig = std::fs::read_to_string(&intro).expect("read the doc file");
        std::fs::write(
            &intro,
            format!("{orig}- type: prose\n  parent: {section}\n  body: {body}\n"),
        )
        .expect("write the doc file");
    };
    let human = |args: &[&str]| {
        let mut a = vec!["--no-daemon", "--json"];
        a.extend_from_slice(args);
        s.json(&a)
    };
    add_prose("Made by a person.");
    let (ok, v) = human(&["add", "guides/intro.yaml"]);
    assert!(ok, "the person stages the doc: {v}");
    let (ok, person) = human(&["commit", "-m", "create"]);
    assert!(ok, "the person's commit succeeds: {person}");
    assert_eq!(person["briefing"]["covers"], 1, "{person}");
    assert!(person["briefing"]["token"].is_string(), "{person}");
    assert!(
        person["cited_briefing"]["base"]["token"].is_string(),
        "{person}"
    );
    let agent = |args: &[&str]| {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        s.json_as("agent:qa", Some("r1"), &a)
    };
    let (ok, v) = agent(&["checkout"]);
    assert!(ok, "the agent checks out its view: {v}");
    add_prose("Made by an agent.");
    let (ok, v) = agent(&["add", "guides/intro.yaml"]);
    assert!(ok, "the agent stages the doc: {v}");
    let (ok, by_agent) = agent(&["commit", "-m", "create"]);
    assert!(ok, "the agent's commit succeeds: {by_agent}");
    assert_eq!(by_agent["briefing"]["covers"], 1, "{by_agent}");
    assert!(by_agent["briefing"]["token"].is_string(), "{by_agent}");
    assert!(by_agent.get("cited_briefing").is_none(), "{by_agent}");
    s.stop();
    json!({"human": shape(&person), "agent": shape(&by_agent)})
}

// ---- signatures ----

fn signatures_row() -> Value {
    use rdy_engine::signature_check::check_signature;
    let mut vectors = Map::new();
    for (name, sk) in [("plain_ed25519", false), ("security_key", true)] {
        let v = rdy_qa::signing::vector(sk);
        let c = check_signature(&v.record, &v.context, Some(&v.trust));
        assert_eq!(c.code, None, "the existing vector verifies: {c:?}");
        vectors.insert(
            name.into(),
            json!({
                "statement": String::from_utf8_lossy(&v.record.statement),
                "namespace": v.record.namespace,
                "subject": v.record.subject.to_string(),
                "signature_id": v.record.id().to_string(),
                "sshsig_len": v.record.sig.len(),
                "sshsig_blake3": blake3_of(&v.record.sig),
                "signed_by": {"actor": v.record.signed_by.actor, "role": format!("{:?}", v.record.signed_by.role)},
                "key": c.key,
                "check_code": c.code,
            }),
        );
    }
    // the statement grammar, built from fixed inputs for every source and version
    let by = |actor: &str, run: Option<&str>| Provenance {
        actor: actor.into(),
        run: run.map(str::to_string),
        role: Role::Approver,
        at_ms: 1,
    };
    let mut landing = match rdy_qa::signing::vector(false).context.subject {
        rdy_engine::signature_check::SignatureSubject::Landing { record, .. } => record,
        other => panic!("the vector signs a landing: {other:?}"),
    };
    let repo = RepoId([0x14; 16]);
    let key = "SHA256:8nEdi2dFYm+E9JRNIKxJNA5RGKdcAaVkhr4NVgAnE/4";
    let text = |b: Vec<u8>| String::from_utf8(b).expect("a statement is UTF-8");
    let mut statements = Map::new();
    statements.insert(
        "approval_local".into(),
        json!(text(
            statement(StatementSource::Approval(&landing), Some(repo), key).unwrap()
        )),
    );
    landing.assurance = Assurance::Relayed;
    landing.relayed_by = Some(ActorRun {
        actor: "agent:qa".into(),
        run: Some("r1".into()),
    });
    landing.relay_evidence = Some(rdy_model::RelayEvidence {
        quoted: "ship it".into(),
        session_ref: "session-1".into(),
        question: None,
    });
    landing.evidence = Some(rdy_model::ApprovalEvidence {
        kind: rdy_model::EvidenceKind::Text,
        options: vec![],
        choice: None,
        card_hash: None,
    });
    statements.insert(
        "approval_relayed".into(),
        json!(text(
            statement(StatementSource::Approval(&landing), Some(repo), key).unwrap()
        )),
    );
    for v in [1u8, 2] {
        let b = rdy_model::signature::statement_for_version(
            StatementSource::RelayApproval(&landing),
            Some(repo),
            key,
            v,
        )
        .unwrap();
        statements.insert(format!("relay_approve_v{v}"), json!(text(b)));
    }
    statements.insert(
        "relay_evidence_hash".into(),
        json!(
            rdy_model::signature::relay_evidence_hash(&landing)
                .unwrap()
                .to_string()
        ),
    );
    let tag = rdy_model::Tag {
        name: "v1.0.0".into(),
        landing: Hash([4; 32]),
        message: None,
        tagged_by: by("human:qa", None),
    };
    statements.insert(
        "tag".into(),
        json!(text(
            statement(
                StatementSource::Tag {
                    hash: tag.id(),
                    tag: &tag
                },
                Some(repo),
                key
            )
            .unwrap()
        )),
    );
    let relay_answer = Answer {
        item: "tag:v1.0.0".into(),
        act: AnswerAct::Tag,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![],
        person: by("human:qa", None),
        relayed_by: Some(ActorRun {
            actor: "agent:qa".into(),
            run: Some("r1".into()),
        }),
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    };
    for v in [1u8, 2] {
        let b = rdy_model::signature::statement_for_version(
            StatementSource::RelayTag {
                hash: tag.id(),
                tag: &tag,
                answer_hash: Hash([5; 32]),
                answer: &relay_answer,
            },
            Some(repo),
            key,
            v,
        )
        .unwrap();
        statements.insert(format!("relay_tag_v{v}"), json!(text(b)));
    }
    statements.insert(
        "keybind".into(),
        json!(text(
            statement(
                StatementSource::Keybind {
                    account: "acct-1",
                    challenge: "challenge-1"
                },
                None,
                key
            )
            .unwrap()
        )),
    );
    let namespaces: Map<String, Value> = [
        StatementKind::Approval,
        StatementKind::Relay,
        StatementKind::Tag,
        StatementKind::Keybind,
    ]
    .into_iter()
    .map(|k| (k.as_str().to_string(), json!(k.namespace())))
    .collect();
    json!({
        "sources": ["rdy_model::signature", "rdy_qa::signing vectors", "rdy_engine::signature_check"],
        "algorithm": "ssh-ed25519 and sk-ssh-ed25519@openssh.com in an SSHSIG envelope with sha512",
        "envelope": "SSHSIG blob over the statement bytes, in the namespace of the statement kind",
        "key_representation": "SHA256:<43 base64 characters> fingerprint of the OpenSSH public key",
        "statement_header": "rdy-statement: <1 | 2>",
        "namespaces": namespaces,
        "limits": {
            "statement": rdy_model::signature::MAX_STATEMENT,
            "value": rdy_model::signature::MAX_VALUE,
            "namespace": rdy_model::signature::MAX_NAMESPACE,
            "sig": rdy_model::signature::MAX_SIG,
            "signature": rdy_model::signature::MAX_SIGNATURE,
            "signer_text": rdy_model::signature::MAX_SIGNER_TEXT,
        },
        "statements": statements,
        "vectors": vectors,
    })
}

// ---- the seeded project and the probes ----

/// The project of the probes: a landed spec with a tag, a suspect, an open tension, a pending
/// batch (its approval card, four exceptions and an advice card) and a view with a review
/// request, so no result array of the probes is empty for lack of data.
struct Rich {
    dir: tempfile::TempDir,
    batch: rdy_qa::intake::Batch,
    /// nb4: a prose block with a located unlinked mention, so a briefing of it lists the text.
    mention: String,
    /// A claim that names the same term unlinked: since D-KVQ7F a prose mention is no issue, so
    /// only a normative block still raises the located unlinked-mention issue (`text?` member).
    claim_mention: String,
    /// nb8 (TASK-978 X2, D-YXBXH): the doc `crowd-0` that holds `mention`, so a block created in
    /// it has `mention` as its one baseline section neighbor; and claims `k1` and `k2` of doc
    /// `crowd-1`, so an edit of `k1` has `k2` as its one section neighbor.
    crowd: String,
    k1: String,
}

/// The pack of the probes: one area, so the coverage areas hold an entry.
/// One advisory rule that only a note can fail, so one probe write carries a failure row in
/// `on_change.checks` and no other probe meets it (ck8, D-JQD5X).
const RICH_PACK: &str = "name: rdy-release\nversion: \"1\"\nextends: rdy-standard\nareas:\n  spec:\n    description: the spec doc\n    paths: [spec, spec/**]\nrules:\n  - name: notes-are-claims\n    select: type = note\n    require: type = claim\n    severity: advisory\n    message: a note is not a claim\n    fix: write it as a claim\n";

fn rich_project() -> Rich {
    use rdy_qa::tensions::{put, upd};
    rdy_model::seed_ids_for_tests(14);
    let _clock = rdy_qa::golden::fixture_clock();
    let (dir, mut r, batch) = rdy_qa::intake::batch_project();
    r.tag(
        &rdy_qa::fixtures::approver(),
        "v1.0.0",
        None,
        Some("Release 1".into()),
    )
    .expect("tag the spec");
    // a landed edit of claim A: claim B, pinned to A, is a suspect
    rdy_qa::fixtures::write(
        &mut r,
        "s",
        json!([{"op": "update", "id": batch.spec.a.to_string(), "body": "A holds, now at length."}]),
    )
    .expect("edit A");
    rdy_qa::fixtures::land(&mut r, "s").expect("land the edit");
    // nb4 (D-YXBXH): nine more docs at the top level (a crowded level), and in one of them a
    // prose block that is one question and names a term it is not linked to (a located
    // unlinked mention)
    let mut ops: Vec<Value> = vec![];
    for n in 0..9 {
        ops.push(json!({"op": "create", "type": "doc", "id": format!("$c{n}"),
            "fields": {"name": format!("crowd-{n}"), "title": format!("Crowd {n}"), "doc_type": "page"}}));
    }
    ops.push(
        json!({"op": "create", "type": "term", "parent": "$c0", "fields": {"name": "Ledger"},
        "body": "Holds balances."}),
    );
    ops.push(
        json!({"op": "create", "type": "prose", "id": "$ask", "parent": "$c0",
        "body": "Who keeps the Ledger?"}),
    );
    ops.push(
        json!({"op": "create", "type": "claim", "id": "$cm", "parent": "$c2",
        "body": "The Ledger keeps the balances."}),
    );
    for k in ["$k1", "$k2"] {
        ops.push(
            json!({"op": "create", "type": "claim", "id": k, "parent": "$c1",
            "body": format!("Crowd claim {}.", &k[2..])}),
        );
    }
    let crowded =
        rdy_qa::fixtures::write(&mut r, "crowd", Value::Array(ops)).expect("write the crowd");
    let (mention, claim_mention, crowd, k1) = (
        crowded.created["$ask"].clone(),
        crowded.created["$cm"].clone(),
        crowded.created["$c0"].clone(),
        crowded.created["$k1"].clone(),
    );
    rdy_qa::fixtures::land(&mut r, "crowd").expect("land the crowd");
    // a view that gives the reviewers a request
    put(&mut r, "a", upd(&batch.spec.c, "C contradicts B."));
    r.close().expect("close");
    rdy_qa::packs::use_pack(dir.path(), "rdy-release", RICH_PACK, "");
    std::fs::write(dir.path().join(".rdy/local.yaml"), "actor: human:qa\n").expect("the person");
    rdy_model::unseed_ids_for_tests();
    Rich {
        dir,
        batch,
        mention,
        claim_mention,
        crowd,
        k1,
    }
}

/// What the probes saw, by label: for each, the merged shape of every success and of every
/// refusal, and the refusal codes (MCP) or the exit values (CLI).
#[derive(Default)]
struct Seen(BTreeMap<String, Value>);

impl Seen {
    fn note(&mut self, label: &str, slot: &str, shape: Value) {
        let e = self.0.entry(label.to_string()).or_insert_with(|| json!({}));
        let merged = match e.get(slot) {
            Some(old) => merge_shape(old, &shape),
            None => shape,
        };
        e[slot] = merged;
    }

    fn add_to_set(&mut self, label: &str, slot: &str, item: Value) {
        let e = self.0.entry(label.to_string()).or_insert_with(|| json!({}));
        let mut items: Vec<Value> = e
            .get(slot)
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        if !items.contains(&item) {
            items.push(item);
            items.sort_by_key(Value::to_string);
        }
        e[slot] = Value::Array(items);
    }

    fn into_value(self) -> Value {
        Value::Object(self.0.into_iter().collect())
    }
}

/// One MCP call, noted under `label`.
fn mcp_probe(
    seen: &mut Seen,
    c: &mut rdy_qa::mcp::McpClient,
    tool: &str,
    label: &str,
    args: Value,
) -> rdy_qa::mcp::Reply {
    use rdy_qa::mcp::Reply;
    let reply = c.call(tool, args);
    match &reply {
        Reply::Ok(v) => seen.note(label, "ok", shape(v)),
        Reply::ToolError(v) => {
            seen.note(label, "refused", shape(v));
            seen.add_to_set(label, "refusal_codes", v["code"].clone());
        }
        Reply::RpcError(v) => seen.note(label, "protocol_error", shape(v)),
    }
    reply
}

/// The value of a success, else null.
fn ok_of(reply: &rdy_qa::mcp::Reply) -> Value {
    match reply {
        rdy_qa::mcp::Reply::Ok(v) => v.clone(),
        _ => Value::Null,
    }
}

/// A write tool called with no briefing; when it asks for one (E_BRIEF_REQUIRED, E_BRIEF_STALE)
/// the briefing of `ids` in `view` is fetched and the call repeats with its token. `at` is the
/// path of the parameter that holds the token (`brief`, or `decision.brief`).
fn mcp_briefed(
    seen: &mut Seen,
    c: &mut rdy_qa::mcp::McpClient,
    tool: &str,
    label: &str,
    mut args: Value,
    ids: &[String],
    view: &str,
) -> rdy_qa::mcp::Reply {
    let first = mcp_probe(seen, c, tool, label, args.clone());
    if matches!(first.code(), Some("E_BRIEF_REQUIRED" | "E_BRIEF_STALE")) {
        let b = ok_of(&mcp_probe(
            seen,
            c,
            "rdy_brief",
            "rdy_brief",
            json!({"ids": ids, "scope": view}),
        ));
        let token = b["base"]["token"].clone();
        assert!(token.is_string(), "the briefing holds a token: {b}");
        if tool == "rdy_supersede" {
            args["decision"]["brief"] = token;
        } else {
            args["brief"] = token;
        }
        return mcp_probe(seen, c, tool, label, args);
    }
    first
}

/// One CLI call in the person's environment with `--json`, noted under `label`; the parsed
/// body of stdout (else of stderr), or null.
fn cli_probe(
    seen: &mut Seen,
    dir: &Path,
    env: &[(String, String)],
    label: &str,
    args: &[&str],
) -> Value {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    let (code, out, err) = rdy_in(dir, env, &a);
    seen.add_to_set(label, "exits", json!(code));
    let (slot, body) = if out.trim().is_empty() {
        ("stderr", err.trim())
    } else {
        ("stdout", out.trim())
    };
    match serde_json::from_str::<Value>(body) {
        Ok(v) => {
            seen.note(label, slot, shape(&v));
            v
        }
        Err(_) => {
            seen.note(label, &format!("{slot}_text"), json!("string"));
            Value::Null
        }
    }
}

/// `cli_probe` for agent `actor` (run `run`): the command line of an agent session.
fn agent_cli_probe(
    seen: &mut Seen,
    dir: &Path,
    env: &[(String, String)],
    (actor, run): (&str, &str),
    label: &str,
    args: &[&str],
) -> Value {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.args(["--no-daemon", "--project"])
        .arg(dir)
        .args(&a)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .env("RDY_ACTOR", actor)
        .env("RDY_RUN", run)
        .current_dir(dir);
    let o = output_within(&mut cmd, Duration::from_secs(120)).expect("run rdy");
    let code = o.status.code().unwrap_or(-1);
    seen.add_to_set(label, "exits", json!(code));
    let (out, err) = (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    );
    let (slot, body) = if out.trim().is_empty() {
        ("stderr", err.trim())
    } else {
        ("stdout", out.trim())
    };
    match serde_json::from_str::<Value>(body) {
        Ok(v) => {
            seen.note(label, slot, shape(&v));
            v
        }
        Err(_) => {
            seen.note(label, &format!("{slot}_text"), json!("string"));
            Value::Null
        }
    }
}

/// ck9 (D-JQD5X): the approval loop of a check, through the command line. An agent's typo edit
/// of claim A is proposed; the agent runs a check and asks for an independent one; a reviewer
/// answers the request with a concern and the author answers its own run with a pass; the list
/// shows both verdicts, the person's card carries them with the acceptance option, and the
/// person accepts the concern as blocking. The card and the queue merge into the `pending` shape
/// (the verdict rows, the acceptance option and the held version are additions); the other replies
/// have labels of their own. One process holds the project at a time: each MCP session ends before the
/// next command.
fn check_loop(seen: &mut Seen, dir: &Path, env: &[(String, String)], a: &BlockId) {
    use rdy_qa::tensions::{ag, j_typo, put_j, upd};
    let author = ("agent:chk9", "chk9");
    let (pref, version) = {
        let mut r = rdy_engine::Rdy::open(dir).expect("open the probe project");
        put_j(&mut r, "chk9", upd(a, "A holds, now at length!"), j_typo());
        let p = r
            .propose(&ag("chk9"), "chk9", "Typo in A", Some("Fixes a typo in A."))
            .expect("propose the typo edit");
        assert!(p.ready, "the typo edit is ready: {:#?}", p.issues);
        r.close().expect("close");
        (p.r#ref, p.id)
    };
    let cli = |seen: &mut Seen, who, label: &str, args: &[&str]| {
        agent_cli_probe(seen, dir, env, who, label, args)
    };
    cli(seen, author, "review checks", &["review", "checks"]);
    let run = cli(
        seen,
        author,
        "review run",
        &["review", "run", "meaning_preservation", &pref],
    );
    assert!(run["request"].is_string(), "the author's run: {run}");
    let asked = cli(
        seen,
        author,
        "review request",
        &[
            "review",
            "request",
            "meaning_preservation",
            &pref,
            "--reason",
            "Does the edit keep the meaning?",
        ],
    );
    assert!(
        asked["request"].is_string(),
        "the independent request: {asked}"
    );
    // the reviewer claims the independent request and answers it with a concern
    let session =
        |actor: &str, run: &str, role: &str| json!({"actor": actor, "run": run, "role": role});
    let mut rv = isolated_mcp(dir);
    rv.call("rdy_session", session("agent:rev9", "rev9", "reviewer"))
        .ok();
    let next = rv
        .call("rdy_review_next", json!({"kind": "meaning_preservation"}))
        .ok();
    assert_eq!(next["request"], asked["request"], "{next}");
    rv.call(
        "rdy_review_submit",
        json!({"request": next["request"], "case_hash": next["case_hash"], "verdict": "concern",
               "citations": [{"block": a.to_string(), "note": "the text moved"}],
               "reason": "The edit changes more than a typo."}),
    )
    .ok();
    drop(rv);
    // the author answers its own run with a pass
    let mut w = isolated_mcp(dir);
    w.call("rdy_session", session("agent:chk9", "chk9", "writer"))
        .ok();
    w.call(
        "rdy_review_submit",
        json!({"request": run["request"], "case_hash": run["case_hash"], "verdict": "pass"}),
    )
    .ok();
    drop(w);
    cli(seen, author, "review list", &["review", "list"]);
    // the person's card carries both verdicts and an acceptance option
    let queue = cli_probe(seen, dir, env, "pending", &["pending"]);
    let card = queue["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|i| i["ref"] == pref.as_str() && i["kind"] == "approve")
        })
        .cloned()
        .expect("the version's approve card");
    let accept = card["check_verdicts"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["verdict"] == "concern"))
        .map(|r| format!("accept_verdict:{}:{version}", r["review"].as_str().unwrap()))
        .expect("the concern is on the card");
    assert!(
        card["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["id"] == accept.as_str()),
        "{card}"
    );
    // the person accepts it as blocking; every lander then refuses the version
    let accepted = cli_probe(
        seen,
        dir,
        env,
        "answer accept_verdict",
        &[
            "answer",
            &pref,
            &accept,
            "--card",
            card["card_hash"].as_str().unwrap(),
        ],
    );
    assert_eq!(accepted["act"], "accept_verdict", "{accepted}");
    let refused = cli_probe(seen, dir, env, "approve held", &["approve", &pref]);
    assert_eq!(refused["code"], "E_PROPOSAL_NOT_READY", "{refused}");
    let held = cli_probe(seen, dir, env, "pending", &["pending"]);
    assert!(
        held["not_ready"]
            .as_array()
            .is_some_and(|n| n.iter().any(|n| n["ref"] == pref.as_str())),
        "{held}"
    );
}

// ---- cli ----

/// The commands the runtime probes do not run, by path (a top-level name stands for the command
/// and everything under it), each with the reason. Every other command of the `cli.tree` is
/// probed (`ok14_probes_account_for_every_command`).
const CLI_NOT_CAPTURED: &[(&str, &str)] = &[
    (
        "add",
        "working copy: the working_copy row records its replies",
    ),
    (
        "alt",
        "needs two open views and a question; its MCP twin is listed too",
    ),
    ("auth", "signs in to a hosted service"),
    ("check", "checks a file; the config row records its refusal"),
    (
        "checkout",
        "working copy: the working_copy row records its replies",
    ),
    (
        "clone",
        "writes a second project; the dump_load row covers the bytes",
    ),
    (
        "commit",
        "working copy: the working_copy row records its replies",
    ),
    (
        "dispute",
        "a person's act on a landing; cli_dispute pins its replies and lines",
    ),
    ("dump", "writes a file; the dump_load row covers the bytes"),
    (
        "edit",
        "working copy: the working_copy row records its replies",
    ),
    (
        "export",
        "writes a file tree; the markdown_exports row covers it",
    ),
    (
        "import",
        "reads a directory of YAML; the working copy and sync rows cover the formats",
    ),
    ("init", "makes a project; every probe starts from one"),
    ("intent renew", "the same reply as post"),
    ("key bind", "changes the signing key"),
    ("key retire", "changes the signing key"),
    ("key revoke", "changes the signing key"),
    ("keys", "assigns keys to people"),
    (
        "land",
        "needs an authority policy that accepts the version; gate_policy_land pins its replies",
    ),
    (
        "load",
        "replaces a project from a dump; the dump_load row covers it",
    ),
    ("mcp", "starts the MCP server; the mcp row covers it"),
    ("pack", "checks a pack file; the schema_packs row covers it"),
    ("plan confirm", "needs an open plan item"),
    ("new finding", "needs a finding pack"),
    (
        "new instruction",
        "writes an instruction; cli_instructions pins its replies",
    ),
    ("new profile", "needs a publication profile"),
    ("new source", "needs a source file"),
    ("plan defer", "needs an open plan item"),
    ("plan dismiss", "needs an open plan item"),
    ("plan done", "needs an open plan item"),
    (
        "policy",
        "writes a decision and a policy block and proposes them; mcp_policy_init pins its replies",
    ),
    ("publish", "uploads a site to a host"),
    (
        "reaffirm",
        "needs a suspect decision; the write op of the same name is an MCP write",
    ),
    ("redact", "destroys content; no dry run is probed"),
    (
        "reindex",
        "rebuilds derived state; the derived_identities row covers it",
    ),
    ("remote add", "edits the remotes of the project"),
    ("remote rm", "edits the remotes of the project"),
    ("remote set-mode", "edits the remotes of the project"),
    (
        "render",
        "writes a site to a directory; the site_urls row covers its routes",
    ),
    ("render-stream", "renders a stream file"),
    ("repin", "needs a pinned reference to move"),
    ("serve", "starts a daemon"),
    ("sign", "needs a signing key"),
    ("site", "starts or reads the live site"),
    ("stream", "checks a stream file; the sync row covers it"),
    (
        "update",
        "working copy: the working_copy row records its replies",
    ),
    ("view compare", "the same reply as the MCP compare"),
    ("view exclude", "needs an included change"),
    ("view fork", "the same reply as new"),
    ("view include", "needs another view's change"),
    ("view own", "changes who owns a view"),
    ("view replay", "needs excluded changes"),
    ("view resolve", "needs a view with a conflict"),
    (
        "worktree",
        "working copy: the working_copy row records its replies",
    ),
];

/// relc4: a project for the publication check probes. The public profile `plan` selects four
/// docs: `guide` (shown prose and one planned note), the planned doc `ghost` with one prose, the
/// empty planned doc `nothing`, and `late`. The public profile `quiet` selects `calm`, which
/// hides nothing. `v1.0.0` is tagged; a later landing plans one more note in `guide` and marks
/// `late` planned, so `plan` hides more than at the tag. Closed again.
fn publication_project() -> tempfile::TempDir {
    use rdy_qa::fixtures::{approver, land, write};
    let (dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$ghost", "fields": {"name": "ghost", "title": "Ghost", "doc_type": "page", "planned": true}},
            {"op": "create", "type": "doc", "id": "$nothing", "fields": {"name": "nothing", "title": "Nothing", "doc_type": "page", "planned": true}},
            {"op": "create", "type": "doc", "id": "$late", "fields": {"name": "late", "title": "Late", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$calm", "fields": {"name": "calm", "title": "Calm", "doc_type": "page"}}
        ]),
    )
    .expect("the documents");
    let id = |k: &str| w.created[k].clone();
    let (admin, guide, ghost, nothing, late, calm) = (
        id("$admin"),
        id("$guide"),
        id("$ghost"),
        id("$nothing"),
        id("$late"),
        id("$calm"),
    );
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "parent": guide, "body": "Shown words."},
            {"op": "create", "type": "note", "parent": guide, "body": "Planned note.", "fields": {"planned": true}},
            {"op": "create", "type": "prose", "parent": ghost, "body": "Ghost words."},
            {"op": "create", "type": "prose", "parent": late, "body": "Late words."},
            {"op": "create", "type": "prose", "parent": calm, "body": "Calm words."},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "plan", "title": "Plan", "audience": "public", "select": [guide, ghost, nothing, late]}},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "quiet", "title": "Quiet", "audience": "public", "select": [calm]}}
        ]),
    )
    .expect("the profiles");
    land(&mut r, "work").expect("the first landing");
    r.tag(&approver(), "v1.0.0", None, None).expect("the tag");
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "note", "parent": guide, "body": "Second planned note.", "fields": {"planned": true}},
            {"op": "update", "id": late, "fields": {"planned": true}}
        ]),
    )
    .expect("the second change");
    land(&mut r, "work").expect("the second landing");
    r.close().expect("close");
    dir
}

/// The publication check over the command line: a first publication and a comparison, each for
/// a profile that hides planned blocks and for one that hides none, so a hidden list is seen
/// both empty and with rows.
fn cli_publication_probes(seen: &mut Seen, env: &[(String, String)]) {
    let dir = publication_project();
    for profile in ["plan", "quiet"] {
        cli_probe(
            seen,
            dir.path(),
            env,
            "publication check first",
            &["publication", "check", "--profile", profile],
        );
        cli_probe(
            seen,
            dir.path(),
            env,
            "publication check against",
            &[
                "publication",
                "check",
                "--profile",
                profile,
                "--against",
                "tag:v1.0.0",
            ],
        );
    }
}

/// The facts of the release binary: exit values and the merged JSON field sets of read, write,
/// answer and review commands run against the seeded project.
fn cli_runtime() -> Value {
    let rich = rich_project();
    let b = &rich.batch;
    let dir = rich.dir.path();
    let (cfg, env) = rdy_qa::fixtures::human_env("human:qa");
    let mut seen = Seen::default();
    let (a, c, doc, proposal) = (
        b.spec.a.to_string(),
        b.spec.c.to_string(),
        b.spec.doc.to_string(),
        b.r#ref.clone(),
    );
    let mut p = |label: &str, args: &[&str]| cli_probe(&mut seen, dir, &env, label, args);
    // reads
    for (label, args) in [
        ("status", vec!["status"]),
        ("tags", vec!["tags"]),
        ("ls", vec!["ls"]),
        ("log", vec!["log", "--limit", "3"]),
        ("tensions", vec!["tensions"]),
        ("suspects", vec!["suspects"]),
        ("pending", vec!["pending"]),
        ("coverage", vec!["coverage"]),
        ("fsck", vec!["fsck"]),
        ("compact", vec!["compact", "--dry-run"]),
        ("upgrade", vec!["upgrade"]),
        ("show", vec!["show", &a]),
        ("show", vec!["show", &doc]),
        ("impact", vec!["impact", &a]),
        ("trace", vec!["trace", &a]),
        ("search", vec!["search", "holds"]),
        ("diff", vec!["diff", "batch"]),
        ("q", vec!["q", "type = claim"]),
        ("show landing", vec!["show", "tag:v1.0.0"]),
        ("brief", vec!["brief", &a]),
        ("brief", vec!["brief", &rich.mention]),
        ("brief", vec!["brief", &rich.claim_mention]),
        ("plan", vec!["plan", "--proposal", &proposal]),
        ("config", vec!["config"]),
        ("view ls", vec!["view", "ls"]),
        ("view show", vec!["view", "show", "batch"]),
        ("key show", vec!["key", "show"]),
        ("remote list", vec!["remote", "list"]),
        ("show", vec!["show", "c-ZZZZZ"]),
        ("no-such-command", vec!["no-such-command"]),
    ] {
        p(label, &args);
    }
    // the person answers the four exceptions, then approves the batch
    for _ in 0..4 {
        let queue = p("pending", &["pending"]);
        let item = queue["items"]
            .as_array()
            .and_then(|items| items.iter().find(|i| i["kind"] == "confirm"))
            .cloned()
            .expect("an exception card is pending");
        p(
            "answer",
            &[
                "answer",
                item["ref"].as_str().unwrap(),
                "confirm",
                "--card",
                item["card_hash"].as_str().unwrap(),
                "--note",
                "fine",
            ],
        );
    }
    let queue = p("pending", &["pending"]);
    let advice = queue["items"]
        .as_array()
        .and_then(|items| items.iter().find(|i| i["kind"] == "resolve"))
        .cloned()
        .expect("the advice card is pending");
    // the person wrote the canon side of the tension: a refusal (E_SELF_REVIEW)
    let tension = b_tension(&p("tensions", &["tensions"]));
    p(
        "resolve",
        &["resolve", &tension, "--dismiss", "checked: they agree"],
    );
    p(
        "answer",
        &[
            "answer",
            advice["ref"].as_str().unwrap(),
            "later",
            "--card",
            advice["card_hash"].as_str().unwrap(),
        ],
    );
    // RULING F1: the release pack was set after the batch was proposed, so the first approval
    // refuses with a fresh card (`E_PROPOSAL_MOVED`); the person reviews it and approves again
    let moved = p("approve", &["approve", &proposal]);
    assert_eq!(moved["code"], "E_PROPOSAL_MOVED", "{moved}");
    assert!(moved["corrected"]["card"].is_object(), "{moved}");
    p("approve", &["approve", &proposal]);
    p("tag", &["tag", "v1.1.0", "-m", "Second"]);
    p("log", &["log", "--limit", "2"]);
    // the person's own view
    p("new folder", &["new", "folder", "guides"]);
    p(
        "new doc",
        &[
            "new",
            "doc",
            "guides/intro",
            "--type",
            "page",
            "--title",
            "Intro",
        ],
    );
    p("ls", &["ls", "guides"]);
    p(
        "propose",
        &[
            "propose",
            "--title",
            "Guides",
            "--summary",
            "Adds a guides folder.",
        ],
    );
    let queue = p("pending", &["pending"]);
    if let Some(item) = queue["items"]
        .as_array()
        .and_then(|items| items.iter().find(|i| i["kind"] == "approve"))
    {
        p(
            "answer",
            &[
                "answer",
                item["ref"].as_str().unwrap(),
                "reject",
                "--card",
                item["card_hash"].as_str().unwrap(),
            ],
        );
    }
    p("view new", &["view", "new", "scratch"]);
    p(
        "view close",
        &["view", "close", "scratch", "--reason", "done"],
    );
    let posted = p(
        "intent post",
        &["intent", "post", "--angle", "tidy", "--blocks", &c],
    );
    p("intent list", &["intent", "list"]);
    if let Some(id) = posted["intent"]["id"].as_str() {
        p("intent withdraw", &["intent", "withdraw", id]);
    }
    p("undo", &["undo"]);
    p("inbox", &["inbox"]);
    // ain5: the instructions, with a home of its own, so no real person layer is read
    let home = tempfile::tempdir().expect("a home");
    let home_text = home.path().to_string_lossy().into_owned();
    let mut home_env = env.clone();
    home_env.push(("HOME".to_string(), home_text.clone()));
    home_env.push(("USERPROFILE".to_string(), home_text));
    cli_probe(&mut seen, dir, &home_env, "instructions", &["instructions"]);
    cli_probe(
        &mut seen,
        dir,
        &home_env,
        "instructions full",
        &["instructions", "--full"],
    );
    // agi6: setup, in a project and a home of its own, so it writes nothing the others read
    let (setup_dir, setup_project) = project();
    drop(setup_project);
    let setup_home = tempfile::tempdir().expect("a home");
    let setup_text = setup_home.path().to_string_lossy().into_owned();
    let mut setup_env = env.clone();
    setup_env.push(("HOME".to_string(), setup_text.clone()));
    setup_env.push(("USERPROFILE".to_string(), setup_text));
    cli_probe(&mut seen, setup_dir.path(), &setup_env, "setup", &["setup"]);
    cli_rule_probes(&mut seen);
    cli_publication_probes(&mut seen, &env);
    check_loop(&mut seen, dir, &env, &b.spec.a);
    let (code, out, _) = rdy_in(dir, &env, &["--version"]);
    let version = out.trim().replace(env!("CARGO_PKG_VERSION"), "<version>");
    drop(cfg);
    let not_captured: Map<String, Value> = CLI_NOT_CAPTURED
        .iter()
        .map(|(c, why)| ((*c).to_string(), json!(why)))
        .collect();
    json!({
        "exit_values": {"success": 0, "refusal": 1, "usage": 2},
        "version_text": {"exit": code, "text": version},
        "probes": seen.into_value(),
        "not_probed_commands": not_captured,
    })
}

/// rtc6: `rdy rule`, all four forms, as an agent relaying a person. A project rule lands, is edited
/// and is removed; a repeat of a rule is held back with its issues; refusals; under the card policy
/// the commands stop at the card and the answer form lands the rule; a person's rule goes to the
/// meta project of a world of its own, so no probe reads a real meta override.
fn cli_rule_probes(seen: &mut Seen) {
    use rdy_qa::instructions::World;
    let w = World::new();
    let meta = w.project();
    w.person(meta.path());
    let env = w.env();
    let who = ("agent:rlc", "rlc");
    let words = |session: &'static str| {
        vec![
            "--why",
            "The person asked for it.",
            "--evidence",
            "Please keep this rule.",
            "--relayed-by",
            "agent:rlc",
            "--question",
            "Shall I save this rule?",
            "--session",
            session,
        ]
    };
    let run = |seen: &mut Seen, dir: &Path, label: &str, head: &[&str], session: &'static str| {
        let mut args = head.to_vec();
        args.extend(words(session));
        agent_cli_probe(seen, dir, &env, who, label, &args)
    };
    let dir = w.project();
    let added = run(
        seen,
        dir.path(),
        "rule add",
        &[
            "rule",
            "add",
            "--scope",
            "project",
            "--text",
            "Say what you changed.",
        ],
        "chat-9",
    );
    let id = added["rule"]["id"]
        .as_str()
        .expect("the rule landed")
        .to_string();
    run(
        seen,
        dir.path(),
        "rule edit",
        &[
            "rule",
            "edit",
            &id,
            "--scope",
            "project",
            "--text",
            "Say what you changed and why.",
        ],
        "chat-10",
    );
    // a repeat of the same words is held back with its issues: no card exists for it
    run(
        seen,
        dir.path(),
        "rule add",
        &[
            "rule",
            "add",
            "--scope",
            "project",
            "--text",
            "Say what you changed and why.",
        ],
        "chat-11",
    );
    run(
        seen,
        dir.path(),
        "rule remove",
        &["rule", "remove", &id, "--scope", "project"],
        "chat-12",
    );
    // the refusals: an unknown rule and an answer to no card
    run(
        seen,
        dir.path(),
        "rule remove",
        &["rule", "remove", "ai-00000", "--scope", "project"],
        "chat-13",
    );
    agent_cli_probe(
        seen,
        dir.path(),
        &env,
        who,
        "rule answer",
        &[
            "rule",
            "answer",
            "P-23456@1",
            "approve",
            "--scope",
            "project",
            "--card",
            &"0".repeat(64),
            "--evidence",
            "Yes.",
            "--relayed-by",
            "agent:rlc",
        ],
    );
    // a person's rule, in the world's own meta project
    run(
        seen,
        dir.path(),
        "rule add person",
        &[
            "rule",
            "add",
            "--scope",
            "person",
            "--text",
            "Answer in British English.",
        ],
        "chat-14",
    );

    // the card policy: the commands stop at the card, and the answer form lands the rule
    let cdir = w.project();
    {
        let mut r = rdy_engine::Rdy::open(cdir.path()).expect("open the card project");
        land_rule_policy(
            &mut r,
            json!({"key": "authority-policy", "agent_may_accept": [], "roles": {}, "require": [],
                "owners": [], "rules_from_chat": "card"}),
        );
    }
    let card = run(
        seen,
        cdir.path(),
        "rule add card",
        &[
            "rule",
            "add",
            "--scope",
            "project",
            "--text",
            "Keep one idea per change.",
        ],
        "chat-15",
    );
    assert_eq!(card["state"], "card", "{card}");
    let answer = |seen: &mut Seen, item: &Value, option: &str, hash: &Value| {
        agent_cli_probe(
            seen,
            cdir.path(),
            &env,
            who,
            "rule answer",
            &[
                "rule",
                "answer",
                item.as_str().expect("a ref"),
                option,
                "--scope",
                "project",
                "--card",
                hash.as_str().expect("a hash"),
                "--evidence",
                "Yes, keep it.",
                "--relayed-by",
                "agent:rlc",
                "--question",
                "Shall I land this rule?",
            ],
        )
    };
    let landed = answer(seen, &card["ref"], "approve", &card["card_hash"]);
    let id = landed["rule"]["id"]
        .as_str()
        .expect("the approval landed")
        .to_string();
    // an edit stops at the card, with the confirm card that rests beside it
    let edit = run(
        seen,
        cdir.path(),
        "rule edit card",
        &[
            "rule",
            "edit",
            &id,
            "--scope",
            "project",
            "--text",
            "Keep one idea in each change.",
        ],
        "chat-16",
    );
    assert_eq!(edit["state"], "card", "{edit}");
    let confirm = edit["cards"][0].clone();
    let answered = answer(seen, &confirm["ref"], "confirm", &confirm["card_hash"]);
    assert_eq!(answered["state"], "answered", "{answered}");
    let next = answered["remaining_cards"][0].clone();
    let landed = answer(seen, &next["ref"], "approve", &next["card_hash"]);
    assert_eq!(landed["rule"]["text"], "Keep one idea in each change.");
}

/// The id of the advised semantic tension in a `tensions` reply.
fn b_tension(v: &Value) -> String {
    v["tensions"]
        .as_array()
        .and_then(|t| t.iter().find(|t| t["kind"] == "semantic"))
        .and_then(|t| t["id"].as_str())
        .expect("the advised tension is listed")
        .to_string()
}

// ---- mcp ----

/// The code, message and fix of a refused call.
fn refusal_of(reply: rdy_qa::mcp::Reply) -> Value {
    match reply {
        rdy_qa::mcp::Reply::ToolError(v) => json!({
            "code": v["code"], "message": v["message"], "fix": v["fix"],
        }),
        other => panic!("the call is refused, not {:?}", other.is_ok()),
    }
}

/// The tools whose result fields the row does not record, each with the reason. Every other tool
/// is called against the seeded project; the row pins the tool list and every parameter schema.
const MCP_NOT_CAPTURED: &[(&str, &str)] = &[
    (
        "rdy_accept",
        "takes suggestions from a write or search reply and writes links",
    ),
    ("rdy_asset", "stores a file"),
    ("rdy_auth", "signs in to a hosted service"),
    ("rdy_clone", "writes a second project to disk"),
    (
        "rdy_dispute",
        "a person's act on a landing; mcp_dispute pins its replies",
    ),
    (
        "rdy_dump",
        "writes a file; the dump_load row covers the bytes",
    ),
    (
        "rdy_export",
        "writes a file tree; the markdown_exports row covers it",
    ),
    ("rdy_key_bind", "changes the signing key"),
    ("rdy_key_retire", "changes the signing key"),
    ("rdy_key_revoke", "changes the signing key"),
    (
        "rdy_land",
        "needs an authority policy that accepts the version; gate_policy_land pins its replies",
    ),
    (
        "rdy_load",
        "replaces a project from a dump; the dump_load row covers it",
    ),
    (
        "rdy_policy",
        "writes a decision and a policy block and proposes them; mcp_policy_init pins its replies",
    ),
    ("rdy_publish", "uploads a site to a host"),
    ("rdy_redact", "destroys content; no dry run is probed"),
    ("rdy_remote", "edits the remotes of the project"),
    (
        "rdy_render",
        "writes a site to a directory; the site_urls row covers its routes",
    ),
    ("rdy_sign", "needs a signing key"),
    ("rdy_site", "starts or reads the live site"),
    (
        "rdy_stream_check",
        "checks a stream file; the sync row covers it",
    ),
];

/// The operations of the `op` tools that the probes do not run, as `tool op`, each with the
/// reason. The test `ok14_probes_account_for_every_command` checks each against the tool's
/// parameter text.
const MCP_OPS_NOT_PROBED: &[(&str, &str)] = &[
    (
        "rdy_alternatives create",
        "needs two open views and a question block",
    ),
    ("rdy_alternatives add", "needs an alternative set"),
    ("rdy_alternatives remove", "needs an alternative set"),
    ("rdy_alternatives compare", "needs an alternative set"),
    ("rdy_alternatives abandon", "needs an alternative set"),
    ("rdy_intent renew", "the same reply as post"),
    ("rdy_view include", "needs another view's change"),
    ("rdy_view exclude", "needs an included change"),
    ("rdy_view replay", "needs excluded changes"),
    ("rdy_view own", "changes who owns a view"),
    ("rdy_view resolve", "needs a view with a conflict"),
];

/// Runs the MCP probes: a writer reads the seeded project and answers the exception cards, a
/// reviewer judges requests and dismisses the advice, and the writer approves the batch, tags,
/// writes, decides, supersedes, revokes and proposes.
fn mcp_probes(rich: &Rich, seen: &mut Seen) {
    use rdy_qa::mcp::McpClient;
    let b = &rich.batch;
    let dir = rich.dir.path();
    let (a, c, doc, section) = (
        b.spec.a.to_string(),
        b.spec.c.to_string(),
        b.spec.doc.to_string(),
        b.spec.section.to_string(),
    );
    let words = |extra: Value| {
        let mut v = json!({
            "evidence": "Yes, go ahead.",
            "question": "Shall I record your answer?",
            "session_ref": "chat-7"
        });
        for (k, x) in extra.as_object().expect("an object") {
            v[k] = x.clone();
        }
        v
    };
    let session =
        |who: &str, role: &str| json!({"actor": format!("agent:{who}"), "run": who, "role": role});

    // the writer reads
    let mut w = isolated_mcp(dir);
    let mut batch_session = session("batch", "writer");
    batch_session["areas"] = json!(["spec"]);
    mcp_probe(
        seen,
        &mut w,
        "rdy_session",
        "rdy_session",
        batch_session.clone(),
    );
    for (tool, args) in [
        ("rdy_overview", json!({})),
        ("rdy_schema", json!({})),
        ("rdy_schema", json!({"block_type": "claim"})),
        ("rdy_tags", json!({})),
        ("rdy_suspects", json!({})),
        ("rdy_tensions", json!({"view": "batch"})),
        ("rdy_pending", json!({})),
        ("rdy_coverage", json!({})),
        ("rdy_log", json!({"limit": 3})),
        ("rdy_read", json!({"id": doc})),
        ("rdy_read", json!({"id": c})),
        ("rdy_plan", json!({"proposal": b.r#ref})),
        ("rdy_key_show", json!({})),
        ("rdy_upgrade", json!({"dry_run": true})),
        ("rdy_compact", json!({"dry_run": true})),
        ("rdy_context", json!({"id": a})),
        ("rdy_impact", json!({"id": a})),
        ("rdy_trace", json!({"id": a})),
        ("rdy_search", json!({"query": "holds"})),
        ("rdy_diff", json!({"from": "batch"})),
        ("rdy_query", json!({"filter": "type = claim"})),
        ("rdy_brief", json!({"ids": [a]})),
        (
            "rdy_brief",
            json!({"ids": [rich.mention.clone(), rich.claim_mention.clone()]}),
        ),
        ("rdy_view", json!({"op": "list"})),
        ("rdy_view", json!({"op": "show", "name": "batch"})),
        (
            "rdy_view",
            json!({"op": "compare", "a": "batch", "b": "canon"}),
        ),
        ("rdy_view", json!({"op": "compare", "a": "a", "b": "canon"})),
        ("rdy_alternatives", json!({"op": "list"})),
        ("rdy_context", json!({"id": "c-ZZZZZ"})),
    ] {
        let label = match args["op"].as_str() {
            Some(op) => format!("{tool} {op}"),
            None => tool.to_string(),
        };
        mcp_probe(seen, &mut w, tool, &label, args);
    }
    // a landing's row and its change list (O15): a label of its own, since the object would
    // merge with the log's array
    mcp_probe(
        seen,
        &mut w,
        "rdy_log",
        "rdy_log landing",
        json!({"landing": "tag:v1.0.0"}),
    );
    let tension = b_tension(&ok_of(&mcp_probe(
        seen,
        &mut w,
        "rdy_tensions",
        "rdy_tensions",
        json!({"view": "batch"}),
    )));
    // intents and views
    let posted = ok_of(&mcp_probe(
        seen,
        &mut w,
        "rdy_intent",
        "rdy_intent post",
        json!({"op": "post", "angle": "tidy", "blocks": [c]}),
    ));
    mcp_probe(
        seen,
        &mut w,
        "rdy_intent",
        "rdy_intent list",
        json!({"op": "list"}),
    );
    mcp_probe(
        seen,
        &mut w,
        "rdy_view",
        "rdy_view create",
        json!({"op": "create", "name": "scratch"}),
    );
    // the person answers the four exceptions
    for _ in 0..4 {
        let queue = ok_of(&mcp_probe(
            seen,
            &mut w,
            "rdy_pending",
            "rdy_pending",
            json!({}),
        ));
        let item = queue["items"]
            .as_array()
            .and_then(|items| items.iter().find(|i| i["kind"] == "confirm"))
            .cloned()
            .expect("an exception card is pending");
        mcp_probe(
            seen,
            &mut w,
            "rdy_answer",
            "rdy_answer",
            words(json!({"ref": item["ref"], "option": "confirm", "card_hash": item["card_hash"]})),
        );
    }
    // a refused answer: the card moved
    mcp_probe(
        seen,
        &mut w,
        "rdy_answer",
        "rdy_answer",
        words(json!({"ref": b.r#ref, "option": "approve", "card_hash": "0".repeat(64)})),
    );
    drop(w);

    // a reviewer dismisses the advice (the person wrote the other side, so the writer cannot)
    let mut rv = isolated_mcp(dir);
    mcp_probe(
        seen,
        &mut rv,
        "rdy_session",
        "rdy_session",
        session("rev", "reviewer"),
    );
    mcp_probe(seen, &mut rv, "rdy_tensions", "rdy_tensions", json!({}));
    mcp_probe(
        seen,
        &mut rv,
        "rdy_resolve",
        "rdy_resolve",
        json!({"tension": tension, "dismiss": {"reason": "checked: they agree"}}),
    );
    drop(rv);

    // the writer approves, tags and writes
    let mut w = isolated_mcp(dir);
    mcp_probe(seen, &mut w, "rdy_session", "rdy_session", batch_session);
    let queue = ok_of(&mcp_probe(
        seen,
        &mut w,
        "rdy_pending",
        "rdy_pending",
        json!({}),
    ));
    let approval = queue["items"]
        .as_array()
        .and_then(|items| items.iter().find(|i| i["kind"] == "approve"))
        .cloned()
        .expect("the approval card is pending");
    mcp_probe(
        seen,
        &mut w,
        "rdy_answer",
        "rdy_answer",
        words(json!({
            "ref": approval["ref"],
            "option": approval["options"][0]["id"],
            "card_hash": approval["card_hash"],
        })),
    );
    mcp_probe(
        seen,
        &mut w,
        "rdy_tag",
        "rdy_tag",
        words(json!({"version": "v1.1.0", "message": "Second"})),
    );
    if let Some(id) = posted["intent"]["id"].as_str() {
        mcp_probe(
            seen,
            &mut w,
            "rdy_intent",
            "rdy_intent withdraw",
            json!({"op": "withdraw", "intent": id}),
        );
    }
    mcp_probe(
        seen,
        &mut w,
        "rdy_view",
        "rdy_view close",
        json!({"op": "close", "name": "scratch", "reason": "done"}),
    );
    // an edit of the claim that view `a` also edits: a collision, its tension in the reply
    mcp_briefed(
        seen,
        &mut w,
        "rdy_write",
        "rdy_write",
        json!({"view": "side", "ops": [{"op": "update", "id": c, "body": "C stands, in a new way."}]}),
        std::slice::from_ref(&c),
        "side",
    );
    let created = ok_of(&mcp_probe(
        seen,
        &mut w,
        "rdy_write",
        "rdy_write",
        json!({"view": "side", "ops": [
            {"op": "create", "type": "claim", "id": "$n", "parent": section, "body": "Side notes stay short."}
        ]}),
    ));
    // nb8: the neighbor row of each unchanged block of C's section names the least changed
    // block, C (edited: the row has `values`) or the new claim (created: none), and the claim's
    // id is not seeded. So the version also edits k1 (its neighbor k2 names k1 alone) and
    // creates a claim in crowd-0 (its neighbor, the mention, names that claim alone): its rows
    // always hold both kinds, whatever the ids
    mcp_probe(
        seen,
        &mut w,
        "rdy_write",
        "rdy_write",
        json!({"view": "side", "ops": [
            {"op": "update", "id": rich.k1, "body": "Crowd claim one, now."},
            {"op": "create", "type": "claim", "id": "$n", "parent": rich.crowd, "body": "Crowd notes stay short."}
        ]}),
    );
    assert!(
        created["created"]["$n"].is_string(),
        "the write created a claim: {created}"
    );
    let decided = ok_of(&mcp_probe(
        seen,
        &mut w,
        "rdy_decide",
        "rdy_decide",
        json!({
            "view": "side", "statement": "Side notes end at one line",
            "title": "Side notes end at one line",
            "why": "Side notes end at one line: the person decides it",
            "reversibility": "two_way", "affects": [c], "parent": section,
        }),
    ));
    let first = decided["created"]["$decision"].as_str();
    let briefed_ids = vec![c.clone(), section.clone()];
    let decided = if first.is_some() {
        decided
    } else {
        ok_of(&mcp_briefed(
            seen,
            &mut w,
            "rdy_decide",
            "rdy_decide",
            json!({
                "view": "side", "statement": "Side notes end at one line",
                "title": "Side notes end at one line",
                "why": "Side notes end at one line: the person decides it",
                "reversibility": "two_way", "affects": [c], "parent": section,
            }),
            &briefed_ids,
            "side",
        ))
    };
    let decision = decided["created"]["$decision"]
        .as_str()
        .expect("the decision was recorded")
        .to_string();
    let superseded = ok_of(&mcp_briefed(
        seen,
        &mut w,
        "rdy_supersede",
        "rdy_supersede",
        json!({
            "old": decision,
            "decision": {
                "view": "side", "statement": "Side notes end at two lines",
                "title": "Side notes end at two lines",
                "why": "Side notes end at two lines: the person decides it",
                "reversibility": "two_way", "parent": section,
            },
        }),
        &briefed_ids,
        "side",
    ));
    let newest = superseded["created"]["$decision"]
        .as_str()
        .map(str::to_string);
    if let Some(newest) = newest {
        mcp_briefed(
            seen,
            &mut w,
            "rdy_revoke",
            "rdy_revoke",
            json!({"view": "side", "target": newest, "reason": "the rule is dropped", "parent": section}),
            &briefed_ids,
            "side",
        );
    }
    mcp_probe(
        seen,
        &mut w,
        "rdy_view",
        "rdy_view fork",
        json!({"op": "fork", "from": "side", "name": "side2"}),
    );
    mcp_probe(
        seen,
        &mut w,
        "rdy_view",
        "rdy_view undo",
        json!({"op": "undo", "view": "side2"}),
    );
    mcp_probe(
        seen,
        &mut w,
        "rdy_propose",
        "rdy_propose",
        json!({"view": "side", "title": "Side notes", "summary": "Adds a short side note and a rule about it."}),
    );
    let queue = ok_of(&mcp_probe(
        seen,
        &mut w,
        "rdy_pending",
        "rdy_pending",
        json!({}),
    ));
    if let Some(item) = queue["items"]
        .as_array()
        .and_then(|items| items.iter().find(|i| i["kind"] == "approve"))
    {
        mcp_probe(
            seen,
            &mut w,
            "rdy_answer",
            "rdy_answer",
            words(json!({"ref": item["ref"], "option": "reject", "card_hash": item["card_hash"]})),
        );
    }
    for (tool, args) in [
        ("rdy_inbox", json!({})),
        ("rdy_inbox", json!({"person": true})),
        ("rdy_log", json!({"limit": 2})),
        ("rdy_tags", json!({})),
        ("rdy_suspects", json!({})),
        ("rdy_coverage", json!({})),
        ("rdy_plan", json!({"view": "side"})),
    ] {
        mcp_probe(seen, &mut w, tool, tool, args);
    }
    drop(w);

    // a reviewer judges requests: a tension with advice to block, a settled request, no tension
    let mut rv = isolated_mcp(dir);
    mcp_probe(
        seen,
        &mut rv,
        "rdy_session",
        "rdy_session",
        session("rev", "reviewer"),
    );
    for (i, verdict) in ["tension", "no_tension", "no_tension"].iter().enumerate() {
        let next = ok_of(&mcp_probe(
            seen,
            &mut rv,
            "rdy_review_next",
            "rdy_review_next",
            json!({"kind": "tension"}),
        ));
        if !next["request"].is_string() {
            break;
        }
        let mut args = json!({
            "request": next["request"], "case_hash": next["case_hash"], "verdict": verdict,
        });
        if i == 0 {
            args["citations"] = json!([{
                "block": next["case"]["blocks"][0]["id"],
                "note": "these two cannot both hold"
            }]);
            args["severity"] = json!("blocking");
            args["reason"] = json!("it contradicts the gate rule");
        }
        mcp_probe(
            seen,
            &mut rv,
            "rdy_review_submit",
            "rdy_review_submit",
            args.clone(),
        );
        if i == 0 {
            // the same request again is stale
            mcp_probe(
                seen,
                &mut rv,
                "rdy_review_submit",
                "rdy_review_submit",
                args,
            );
        }
    }
    mcp_probe(
        seen,
        &mut rv,
        "rdy_review_next",
        "rdy_review_next",
        json!({"kind": "mention"}),
    );
    mcp_probe(seen, &mut rv, "rdy_tensions", "rdy_tensions", json!({}));
    drop(rv);
    // the check surfaces (ck8, D-JQD5X): a writer writes a note that the pack rule flags, so
    // its on_change carries a failure row, records a proposed decision, runs a check on it and
    // asks for an independent review; a reviewer answers it, and the writer answers its own
    // run. These calls come last, so the tension probes above do not meet the new view. One
    // server holds the project at a time, so each session ends before the next starts.
    let mut ck = isolated_mcp(dir);
    mcp_probe(
        seen,
        &mut ck,
        "rdy_session",
        "rdy_session",
        session("chk", "writer"),
    );
    let note = ok_of(&mcp_briefed(
        seen,
        &mut ck,
        "rdy_write",
        "rdy_write",
        json!({"ops": [
            {"op": "create", "type": "note", "id": "$k", "parent": section, "body": "Checks are authoring tools."}
        ]}),
        std::slice::from_ref(&section),
        "chk",
    ));
    assert!(
        note["on_change"]["checks"][0]["rule"] == "notes-are-claims",
        "the note fails the pack rule in on_change.checks: {note}"
    );
    // the decision is written without a probe: the other views' decisions pin that reply's shape,
    // and this one would only fill their empty arrays
    let briefed_ids = vec![c.clone(), section.clone()];
    let mut decide = json!({
        "statement": "Checks never gate", "title": "Checks never gate",
        "why": "A person decides whether a check gates",
        "reversibility": "two_way", "affects": [c], "parent": section,
    });
    let mut proposed = ck.call("rdy_decide", decide.clone());
    if matches!(proposed.code(), Some("E_BRIEF_REQUIRED" | "E_BRIEF_STALE")) {
        let brief = ck
            .call("rdy_brief", json!({"ids": briefed_ids, "scope": "chk"}))
            .ok();
        decide["brief"] = brief["base"]["token"].clone();
        proposed = ck.call("rdy_decide", decide);
    }
    let proposed = proposed.ok();
    let subject = proposed["created"]["$decision"]
        .as_str()
        .expect("the decision was recorded")
        .to_string();
    mcp_probe(
        seen,
        &mut ck,
        "rdy_review",
        "rdy_review checks",
        json!({"op": "checks"}),
    );
    let run = ok_of(&mcp_probe(
        seen,
        &mut ck,
        "rdy_review",
        "rdy_review run",
        json!({"op": "run", "check": "decision_review", "subject": subject}),
    ));
    mcp_probe(
        seen,
        &mut ck,
        "rdy_review",
        "rdy_review request",
        json!({"op": "request", "check": "decision_review", "subject": subject,
               "reason": "Check the basis."}),
    );

    drop(ck);
    // the independent request: a concern with its reason and citations. Each verdict has a label
    // of its own, so the tension probes above keep their shapes.
    let mut rv = isolated_mcp(dir);
    mcp_probe(
        seen,
        &mut rv,
        "rdy_session",
        "rdy_session",
        session("rev", "reviewer"),
    );
    let next = ok_of(&mcp_probe(
        seen,
        &mut rv,
        "rdy_review_next",
        "rdy_review_next decision_review",
        json!({"kind": "decision_review"}),
    ));
    assert!(
        next["request"].is_string(),
        "the independent request is queued: {next}"
    );
    mcp_probe(
        seen,
        &mut rv,
        "rdy_review_submit",
        "rdy_review_submit concern",
        json!({"request": next["request"], "case_hash": next["case_hash"], "verdict": "concern",
               "citations": [{"block": subject, "note": "the basis names no measure"}],
               "reason": "The decision names no measure."}),
    );
    drop(rv);
    // the author's own run: a pass; then both requests are answered, and the list shows each verdict
    let mut ck = isolated_mcp(dir);
    mcp_probe(
        seen,
        &mut ck,
        "rdy_session",
        "rdy_session",
        session("chk", "writer"),
    );
    mcp_probe(
        seen,
        &mut ck,
        "rdy_review_submit",
        "rdy_review_submit pass",
        json!({"request": run["request"], "case_hash": run["case_hash"], "verdict": "pass"}),
    );
    mcp_probe(
        seen,
        &mut ck,
        "rdy_review",
        "rdy_review list",
        json!({"op": "list"}),
    );
    drop(ck);
    // the import (ui6, D-Y9DNZ): a writer imports a format-3 tree into its own view and a
    // misspelt key is refused; a reader is refused. These calls come last, so no probe above
    // meets the new view.
    let tree = tempfile::tempdir().expect("a tree directory");
    write_import_tree(tree.path());
    let tree_dir = tree.path().to_string_lossy().into_owned();
    let mut im = isolated_mcp(dir);
    mcp_probe(
        seen,
        &mut im,
        "rdy_session",
        "rdy_session",
        session("imp", "writer"),
    );
    mcp_probe(
        seen,
        &mut im,
        "rdy_import",
        "rdy_import unknown key",
        json!({"dri": tree_dir, "view": "imp"}),
    );
    let imported = ok_of(&mcp_probe(
        seen,
        &mut im,
        "rdy_import",
        "rdy_import",
        json!({"dir": tree_dir, "view": "imp"}),
    ));
    assert!(
        imported["created"].is_object(),
        "the writer's import succeeds: {imported}"
    );
    drop(im);
    let mut rdr = isolated_mcp(dir);
    mcp_probe(
        seen,
        &mut rdr,
        "rdy_session",
        "rdy_session",
        session("rdr", "reader"),
    );
    mcp_probe(
        seen,
        &mut rdr,
        "rdy_import",
        "rdy_import reader",
        json!({"dir": tree_dir, "view": "rdr"}),
    );
    drop(rdr);
    // ain4: the instructions, with a home of its own, so no real person layer is read; a load
    // id that no layer holds is refused
    let home = tempfile::tempdir().expect("a home");
    let mut ins = McpClient::start_isolated(dir, home.path());
    mcp_probe(
        seen,
        &mut ins,
        "rdy_session",
        "rdy_session",
        session("ins", "writer"),
    );
    mcp_probe(
        seen,
        &mut ins,
        "rdy_instructions",
        "rdy_instructions",
        json!({}),
    );
    mcp_probe(
        seen,
        &mut ins,
        "rdy_instructions",
        "rdy_instructions full",
        json!({"full": true}),
    );
    mcp_probe(
        seen,
        &mut ins,
        "rdy_instructions",
        "rdy_instructions unknown load",
        json!({"load": ["project:d-ZZZZZ"]}),
    );
    drop(ins);
    rule_probes(seen);
    elicitation_probes(seen);
    let _ = (doc, a);
}

/// Lands in the closed store `r` a doc with the authority policy `fields` and the decision that
/// adopts it.
fn land_rule_policy(r: &mut rdy_engine::Rdy, fields: Value) {
    use rdy_engine::WriteRequest;
    use rdy_model::Justification;
    use rdy_qa::fixtures::{land, parse_test_ops, write, writer};
    let w = write(
        r,
        "pol",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
    )
    .expect("write the rules");
    r.write(
        &writer(),
        WriteRequest {
            view: "pol".into(),
            ops: parse_test_ops(json!([
                {"op": "create", "type": "authority_policy", "id": "$p", "parent": w.created["$d"], "fields": fields}
            ]))
            .expect("the policy ops"),
            justification: Some(Justification::Implements(vec![
                BlockId::parse(&w.created["$dec"]).expect("a decision id"),
            ])),
            message: String::new(),
        },
    )
    .expect("write the policy");
    land(r, "pol").expect("land the policy");
}

/// rtc5: `rdy_rule`, both input forms. A project rule lands, is edited and is removed, and a
/// repeat of a rule is held back with its issues; under the card policy the call stops at a card
/// and the answer form lands it; a person's rule goes to the meta project of a world of its own,
/// so no probe reads a real meta override.
fn rule_probes(seen: &mut Seen) {
    use rdy_qa::instructions::World;
    let session = json!({"actor": "agent:rl", "run": "rl", "role": "writer"});
    let words = |extra: Value| {
        let mut v = json!({
            "why": "The person asked for it.",
            "evidence": "Please keep this rule.",
            "question": "Shall I save this rule?",
            "session_ref": "chat-9"
        });
        for (k, x) in extra.as_object().expect("an object") {
            v[k] = x.clone();
        }
        v
    };
    let (dir, r) = project();
    drop(r);
    let mut c = isolated_mcp(dir.path());
    mcp_probe(seen, &mut c, "rdy_session", "rdy_session", session.clone());
    let added = ok_of(&mcp_probe(
        seen,
        &mut c,
        "rdy_rule",
        "rdy_rule add",
        words(json!({"scope": "project", "op": "add", "text": "Say what you changed."})),
    ));
    let id = added["rule"]["id"].clone();
    assert!(id.is_string(), "the rule landed: {added}");
    mcp_probe(
        seen,
        &mut c,
        "rdy_rule",
        "rdy_rule edit",
        words(json!({"scope": "project", "op": "edit", "id": id,
            "text": "Say what you changed and why.", "session_ref": "chat-10"})),
    );
    // a repeat of the same words is held back with its issues: no card exists for it
    mcp_probe(
        seen,
        &mut c,
        "rdy_rule",
        "rdy_rule add",
        words(
            json!({"scope": "project", "op": "add", "text": "Say what you changed and why.",
            "session_ref": "chat-11"}),
        ),
    );
    mcp_probe(
        seen,
        &mut c,
        "rdy_rule",
        "rdy_rule remove",
        words(json!({"scope": "project", "op": "remove", "id": id, "session_ref": "chat-12"})),
    );
    // the refusals: a missing key, an unknown rule and an answer to no card
    mcp_probe(
        seen,
        &mut c,
        "rdy_rule",
        "rdy_rule add",
        json!({"scope": "project", "op": "add"}),
    );
    mcp_probe(
        seen,
        &mut c,
        "rdy_rule",
        "rdy_rule remove",
        words(
            json!({"scope": "project", "op": "remove", "id": "ai-00000", "session_ref": "chat-13"}),
        ),
    );
    mcp_probe(
        seen,
        &mut c,
        "rdy_rule",
        "rdy_rule answer",
        json!({"scope": "project", "answer": {"ref": "P-ZZZZZ@1", "option": "approve",
            "card_hash": "0".repeat(64), "evidence": "Yes."}}),
    );
    drop(c);
    // a reader writes nothing
    let mut rdr = isolated_mcp(dir.path());
    mcp_probe(
        seen,
        &mut rdr,
        "rdy_session",
        "rdy_session",
        json!({"actor": "agent:rlr", "run": "rlr", "role": "reader"}),
    );
    mcp_probe(
        seen,
        &mut rdr,
        "rdy_rule",
        "rdy_rule add",
        words(json!({"scope": "project", "op": "add", "text": "A reader's rule."})),
    );
    drop(rdr);

    // the card policy: the call stops at the card, and the answer form lands it
    let (cdir, mut r) = project();
    land_rule_policy(
        &mut r,
        json!({"key": "authority-policy", "agent_may_accept": [], "roles": {}, "require": [],
            "owners": [], "rules_from_chat": "card"}),
    );
    drop(r);
    let mut c = isolated_mcp(cdir.path());
    mcp_probe(seen, &mut c, "rdy_session", "rdy_session", session);
    let card = ok_of(&mcp_probe(
        seen,
        &mut c,
        "rdy_rule",
        "rdy_rule card",
        words(json!({"scope": "project", "op": "add", "text": "Keep one idea per change."})),
    ));
    assert_eq!(card["state"], "card", "{card}");
    // the person's answers, relayed: an approval lands the rule
    fn answer(
        seen: &mut Seen,
        c: &mut rdy_qa::mcp::McpClient,
        item: &Value,
        option: &str,
        hash: &Value,
    ) -> Value {
        ok_of(&mcp_probe(
            seen,
            c,
            "rdy_rule",
            "rdy_rule answer",
            json!({"scope": "project", "answer": {"ref": item, "option": option,
                "card_hash": hash, "evidence": "Yes, keep it.",
                "question": "Shall I land this rule?"}}),
        ))
    }
    let landed = answer(seen, &mut c, &card["ref"], "approve", &card["card_hash"]);
    let id = landed["rule"]["id"].clone();
    assert!(id.is_string(), "the approval landed the rule: {landed}");
    // an edit stops at the card, with the confirm card that rests beside it
    let edit = ok_of(&mcp_probe(
        seen,
        &mut c,
        "rdy_rule",
        "rdy_rule card",
        words(json!({"scope": "project", "op": "edit", "id": id,
            "text": "Keep one idea in each change.", "session_ref": "chat-14"})),
    ));
    assert_eq!(edit["state"], "card", "{edit}");
    let confirm = edit["cards"][0].clone();
    assert!(confirm["card_hash"].is_string(), "a confirm card: {edit}");
    let answered = answer(
        seen,
        &mut c,
        &confirm["ref"],
        "confirm",
        &confirm["card_hash"],
    );
    assert_eq!(answered["state"], "answered", "{answered}");
    let next = answered["remaining_cards"][0].clone();
    let landed = answer(seen, &mut c, &next["ref"], "approve", &next["card_hash"]);
    assert_eq!(landed["rule"]["text"], "Keep one idea in each change.");
    drop(c);

    // a person's rule, in a world with a meta project of its own
    let w = World::new();
    let meta = w.project();
    w.person(meta.path());
    let src = w.project();
    let mut c = w.mcp(src.path());
    mcp_probe(
        seen,
        &mut c,
        "rdy_session",
        "rdy_session",
        json!({"actor": "agent:rlp", "run": "rlp", "role": "writer"}),
    );
    mcp_probe(
        seen,
        &mut c,
        "rdy_rule",
        "rdy_rule add person",
        words(json!({"scope": "person", "op": "add", "text": "Answer in British English."})),
    );
    drop(c);
}

/// ui10: the elicit input, its unavailable refusal, and the protocol envelopes and outcomes of a
/// client that states the 2026-07-28 context in its requests.
fn elicitation_probes(seen: &mut Seen) {
    use rdy_qa::fixtures::{approver, project, seed_doc, write, writer};
    use rdy_qa::mcp::DEADLINE;
    let (dir, mut r) = project();
    let (_, section, _) = seed_doc(&mut r, "seed");
    let p = r
        .propose(&writer(), "seed", "Seed", Some("Seeds."))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    write(
        &mut r,
        "one",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "A claim of one."}]),
    )
    .unwrap();
    rdy_qa::intake::answer_open_items(&mut r, &writer(), "one");
    r.propose(&writer(), "one", "One", Some("Adds one claim."))
        .unwrap();
    drop(r);
    std::fs::write(dir.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    let mut c = isolated_mcp(dir.path());
    mcp_probe(
        seen,
        &mut c,
        "rdy_session",
        "rdy_session",
        json!({"actor": "agent:elc", "run": "elc", "role": "writer"}),
    );
    let queue = ok_of(&mcp_probe(
        seen,
        &mut c,
        "rdy_pending",
        "rdy_pending",
        json!({}),
    ));
    let item = queue["items"][0].clone();
    assert!(item.is_object(), "an item waits: {queue}");
    let ask = json!({"ref": item["ref"], "card_hash": item["card_hash"], "elicit": true});
    mcp_probe(
        seen,
        &mut c,
        "rdy_answer",
        "rdy_answer elicit unavailable",
        ask.clone(),
    );
    let mut mixed = ask.clone();
    mixed["option"] = json!("later");
    mcp_probe(seen, &mut c, "rdy_answer", "rdy_answer elicit mixed", mixed);
    let mut raw = |extra: Value| {
        let id = c.next_id();
        let mut args = ask.clone();
        rdy_qa::mcp::supply_kungfu("rdy_answer", &mut args);
        let mut params = json!({"name": "rdy_answer", "arguments": args, "_meta": {
            "rdy/actor": "agent:elc", "rdy/run": "elc",
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {"elicitation": {"form": {}}},
        }});
        for (k, v) in extra.as_object().unwrap() {
            params[k] = v.clone();
        }
        c.send_raw(
            &json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params})
                .to_string(),
        );
        c.recv(id, DEADLINE)["result"].clone()
    };
    let asked = raw(json!({}));
    seen.note("rdy_answer input_required", "ok", shape(&asked));
    let handle = asked["requestState"].clone();
    let first =
        raw(json!({"requestState": handle, "inputResponses": {"answer": {"action": "decline"}}}));
    seen.note("rdy_answer declined", "ok", shape(&first));
    let again =
        raw(json!({"requestState": handle, "inputResponses": {"answer": {"action": "decline"}}}));
    seen.note("rdy_answer elicit used", "refused", shape(&again));
    let asked = raw(json!({}));
    let cancelled = raw(json!({"requestState": asked["requestState"],
        "inputResponses": {"answer": {"action": "cancel"}}}));
    seen.note("rdy_answer cancelled", "ok", shape(&cancelled));
    let asked = raw(json!({}));
    let invalid = raw(json!({"requestState": asked["requestState"],
        "inputResponses": {"answer": {"action": "maybe"}}}));
    seen.note("rdy_answer elicit invalid", "refused", shape(&invalid));
    let later = item["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"] == "later")
        .unwrap_or_else(|| panic!("the first card has Later: {item}"));
    let asked = raw(json!({}));
    let elicited = raw(
        json!({"requestState": asked["requestState"], "inputResponses": {
        "answer": {"action": "accept", "content": {"choice": later["id"]}}}}),
    );
    seen.note("rdy_answer elicited", "ok", shape(&elicited));
}

/// A format-3 export tree at `out`: doc `imported` with a section by agent:a (run r1) at 100 and
/// two decisions by agent:b (run r2) at 200, one accepted and one proposed.
fn write_import_tree(out: &Path) {
    std::fs::create_dir_all(out.join("docs")).expect("docs directory");
    std::fs::write(
        out.join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: work\n",
    )
    .expect("manifest");
    let docs = "\
- id: d-QM001
  type: doc
  order: a1
  fields:
    doc_type: overview
    key: imported
    title: Imported
- id: s-QM001
  type: section
  parent: d-QM001
  order: a1
  fields:
    title: Rulings
- id: D-QMAC1
  type: decision
  parent: s-QM001
  order: a1
  fields:
    needs_content: false
    statement: Decision D-QMAC1 holds
    status: accepted
- id: D-QMPR1
  type: decision
  parent: s-QM001
  order: a2
  fields:
    needs_content: false
    statement: Decision D-QMPR1 holds
    status: proposed
";
    std::fs::write(out.join("docs/imported.yaml"), docs).expect("docs");
    let row = |id: &str, actor: &str, run: &str, at: i64| {
        format!("- id: {id}\n  actor: {actor}\n  run: {run}\n  role: writer\n  at_ms: {at}\n")
    };
    let authors = row("d-QM001", "agent:a", "r1", 100)
        + &row("s-QM001", "agent:a", "r1", 100)
        + &row("D-QMAC1", "agent:b", "r2", 200)
        + &row("D-QMPR1", "agent:b", "r2", 200);
    std::fs::write(out.join("authors.yaml"), authors).expect("authors");
}

/// kfu6 (D-Y18E1): the local kungfu counts as `rdy status` and `rdy_overview` show them, from a
/// project with fixed rows in its count file. The days are named by their distance from today, so
/// the capture does not depend on the date. The two zero replies come first: with no count file
/// neither surface has a member for it.
fn kungfu_counts_row() -> Value {
    use rdy_engine::kungfu_counts::{self as kc, Case, Counts, Day, Hooks, Key, Recorder, Rows};
    let (dir, r) = rdy_qa::fixtures::project();
    drop(r);
    let (cfg, env) = rdy_qa::fixtures::human_env("human:qa");
    let status = |args: &[&str]| rdy_in(dir.path(), &env, args);
    let json_status = || -> Value {
        let (code, out, err) = status(&["--json", "status"]);
        assert_eq!(code, 0, "{err}");
        serde_json::from_str(out.trim()).expect("status JSON")
    };
    let zero = json_status();
    assert!(
        zero.get("kungfu").is_none() && zero.get("kungfu_counts").is_none(),
        "no count, no member: {zero}"
    );

    let today = Day::today();
    let mut rows = Rows::new();
    let mut put = |back: i64, actor: &str, run: Option<&str>, ruleset: &str, c: (u64, u64, u64)| {
        rows.insert(
            Key {
                day: today.minus(back),
                actor: actor.into(),
                run: run.map(Into::into),
                ruleset: ruleset.into(),
            },
            Counts {
                not_loaded: c.0,
                lost: c.1,
                wrong: c.2,
            },
        );
    };
    put(0, "agent:a1", Some("r1"), "write", (2, 1, 0));
    put(0, "agent:a1", Some("r1"), "read", (1, 0, 0));
    put(0, "agent:a2", None, "bootstrap", (0, 2, 1));
    put(5, "agent:a3", Some("r9"), "approvals", (1, 0, 0));
    // the recorder makes the private run directory; the rows are then the file as it is
    let rec = Recorder::detached(&dir.path().join(".rdy"), Hooks::default()).expect("a recorder");
    rec.record("agent:seed", None, "read", Case::Lost);
    while !rec.settled() {
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(rec);
    std::fs::write(
        dir.path().join(".rdy/run").join(kc::FILE_NAME),
        kc::to_bytes(&rows),
    )
    .expect("write the count file");
    let line = "kungfu: 4 not loaded, 3 lost, 1 wrong, in 3 sessions.";

    let by_day = |v: &Value| -> Value {
        let names: Vec<(String, String)> = [0, 5]
            .iter()
            .map(|n| {
                let name = if *n == 0 {
                    "today".to_string()
                } else {
                    format!("today-{n}")
                };
                (today.minus(*n).to_string(), name)
            })
            .collect();
        let mut out = v.clone();
        for row in out.as_array_mut().expect("rows") {
            let day = row["day"].as_str().expect("a day").to_string();
            let name = names.iter().find(|(d, _)| *d == day).expect("a seeded day");
            row["day"] = json!(name.1);
        }
        out
    };
    let seeded = json_status();
    assert_eq!(seeded["kungfu"], line, "{seeded}");
    let counts = by_day(&seeded["kungfu_counts"]);
    let mut row_members: Vec<&String> = seeded["kungfu_counts"][0]
        .as_object()
        .expect("a row")
        .keys()
        .collect();
    row_members.sort();
    let (code, text, err) = status(&["status"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(text.lines().filter(|l| *l == line).count(), 1, "{text}");
    assert_eq!(text.matches("kungfu").count(), 1, "{text}");
    drop(cfg);

    // the overview of a server in the same project: the recorder reads the file on its own
    let mut c = isolated_mcp(dir.path());
    let opened = c.call(
        "rdy_session",
        json!({"actor": "agent:ov", "run": "ov", "role": "writer"}),
    );
    assert!(opened.is_ok(), "{opened:?}");
    let start = std::time::Instant::now();
    let overview = loop {
        let o = c.call("rdy_overview", json!({})).ok();
        if o.get("kungfu").is_some() {
            break o;
        }
        assert!(
            start.elapsed() < rdy_qa::mcp::DEADLINE,
            "the overview never showed the counts: {o}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(overview["kungfu"], line, "{overview}");
    assert!(overview.get("kungfu_counts").is_none(), "{overview}");
    json!({
        "sources": ["rdy status --json against a seeded count file", "rdy_overview against the same project"],
        "template": "kungfu: <not_loaded> not loaded, <lost> lost, <wrong> wrong, in <sessions> sessions.",
        "zero_omits": ["kungfu", "kungfu_counts"],
        "status": {
            "kungfu": line,
            "kungfu_counts": counts,
            "row_members": row_members,
            "text_lines": 1,
        },
        "overview": {"kungfu": overview["kungfu"], "kungfu_counts": null},
    })
}

/// The publication check over the chat tool, as the CLI probes run it.
fn mcp_publication_probes(seen: &mut Seen) {
    let dir = publication_project();
    let mut c = isolated_mcp(dir.path());
    let opened = c.call(
        "rdy_session",
        json!({"actor": "agent:pub", "run": "pub", "role": "reader"}),
    );
    assert!(opened.is_ok(), "the reader session opens");
    for profile in ["plan", "quiet"] {
        mcp_probe(
            seen,
            &mut c,
            "rdy_publication_check",
            "rdy_publication_check first",
            json!({"profile": profile}),
        );
        mcp_probe(
            seen,
            &mut c,
            "rdy_publication_check",
            "rdy_publication_check against",
            json!({"profile": profile, "against": "tag:v1.0.0"}),
        );
    }
}

fn mcp_row() -> Value {
    use rdy_qa::mcp::Reply;
    let rich = rich_project();
    let mut seen = Seen::default();
    mcp_probes(&rich, &mut seen);
    mcp_publication_probes(&mut seen);
    let (d, r) = rdy_qa::fixtures::project();
    drop(r);
    let mut c = isolated_mcp(d.path());
    let mut tools = c.tools();
    tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    assert!(tools.len() > 20, "the server lists its tools");
    fn sort_required(v: &mut Value) {
        match v {
            Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    if k == "required"
                        && let Some(a) = x.as_array_mut()
                    {
                        a.sort_by_key(|e| e.as_str().map(str::to_string));
                    }
                    sort_required(x);
                }
            }
            Value::Array(a) => a.iter_mut().for_each(sort_required),
            _ => {}
        }
    }
    let mut schemas = Map::new();
    for t in &mut tools {
        sort_required(t);
        let name = t["name"].as_str().expect("tool name").to_string();
        schemas.insert(name, json!({"input_schema": t["inputSchema"]}));
    }
    // every tool is probed or listed with a reason, and not both
    let probed: std::collections::BTreeSet<String> = seen
        .0
        .keys()
        .map(|l| l.split(' ').next().unwrap_or_default().to_string())
        .collect();
    for (tool, _) in MCP_NOT_CAPTURED {
        assert!(schemas.contains_key(*tool), "{tool} is not a tool");
        assert!(
            !probed.contains(*tool),
            "{tool} is probed and listed as not captured"
        );
    }
    for name in schemas.keys() {
        assert!(
            probed.contains(name) || MCP_NOT_CAPTURED.iter().any(|(t, _)| t == name),
            "{name} is neither probed nor listed in MCP_NOT_CAPTURED"
        );
    }
    // the refusal envelope: a tool error holds code, message and fix
    let refusal = match c.call("rdy_context", json!({"id": "c-ZZZZZ"})) {
        Reply::ToolError(v) => shape(&v),
        other => panic!("an unknown block is a tool error: {:?}", other.is_ok()),
    };
    drop(c);
    // a fabricated request is no request (E_REVIEW_STALE whatever the verdict), and `check` is
    // an unknown check
    let mut rv = isolated_mcp(d.path());
    let opened = rv.call(
        "rdy_session",
        json!({"actor": "agent:qa", "run": "qa", "role": "reviewer"}),
    );
    assert!(opened.is_ok(), "the reviewer session opens");
    let mut refusals = Map::new();
    for verdict in ["pass", "concern", "fail"] {
        let args =
            json!({"request": "rq-000000000000", "case_hash": "0".repeat(64), "verdict": verdict});
        refusals.insert(
            format!("rdy_review_submit {verdict}"),
            refusal_of(rv.call("rdy_review_submit", args)),
        );
    }
    refusals.insert(
        "rdy_review_next check".into(),
        refusal_of(rv.call("rdy_review_next", json!({"kind": "check"}))),
    );
    let not_captured: Map<String, Value> = MCP_NOT_CAPTURED
        .iter()
        .map(|(t, why)| ((*t).to_string(), json!(why)))
        .collect();
    let ops_not_probed: Map<String, Value> = MCP_OPS_NOT_PROBED
        .iter()
        .map(|(t, why)| ((*t).to_string(), json!(why)))
        .collect();
    json!({
        "sources": ["rdy mcp tools/list", "tools/call against a seeded project"],
        "tool_names": tools.iter().map(|t| t["name"].clone()).collect::<Vec<_>>(),
        "tools": schemas,
        "result_fields": seen.into_value(),
        "result_fields_not_captured": not_captured,
        "ops_not_probed": ops_not_probed,
        "carried_value_refusals": refusals,
        "kungfu_counts": kungfu_counts_row(),
        "refusal_envelope": refusal,
        "refusal_error_rows": rdy_mcp::tools::ERROR_ROWS.iter().map(|(c, _, _)| *c).collect::<Vec<_>>(),
    })
}

// ---- errors and warnings ----

/// The refusal a normal open of a seeded follower gets, with its repo id named by the row's word.
fn follower_refusal() -> Value {
    let d = tempfile::tempdir().expect("a directory");
    let r = rdy_engine::Rdy::init(&d.path().join("p"), "rdy-standard").expect("a project");
    let dump = d.path().join("p.rdydump");
    r.read().dump(&dump).expect("a dump");
    let repo = r.store().repo_id().expect("a repo id").expect("a repo id");
    r.close().expect("a close");
    let follower = d.path().join("f");
    rdy_engine::follower::seed(&dump, &follower).expect("a seed");
    let e = rdy_engine::Rdy::open(&follower)
        .err()
        .expect("a follower refuses a normal open");
    json!({
        "code": e.code,
        "message": e.message.replace(&repo.to_string(), "<repo id>"),
        "fix": e.fix,
    })
}

/// Every `E_` and `W_` code constant in the sources, and every `_FIX` and `_MESSAGE` template
/// constant, by scanning the crates' source text.
fn errors_row() -> Value {
    let consts = Regex::new(
        r#"(?s)const\s+([A-Z][A-Z0-9_]*)\s*:\s*&(?:'static\s+)?str\s*=\s*("(?:[^"\\]|\\.)*")\s*;"#,
    )
    .unwrap();
    let mut codes: BTreeMap<String, String> = BTreeMap::new();
    let mut templates: BTreeMap<String, String> = BTreeMap::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).expect("read the sources") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut files = vec![];
    for krate in std::fs::read_dir(workspace_root().join("crates")).expect("crates") {
        let src = krate.expect("crate").path().join("src");
        if src.is_dir() {
            walk(&src, &mut files);
        }
    }
    files.sort();
    for f in files {
        // test modules and the QA crate are not product text
        if f.components().any(|c| c.as_os_str() == "rdy-qa") {
            continue;
        }
        let text = std::fs::read_to_string(&f).expect("read a source file");
        let product = product_text(&text);
        for m in consts.captures_iter(&product) {
            let (name, value) = (m[1].to_string(), m[2].to_string());
            let template = name.ends_with("_FIX") || name.ends_with("_MESSAGE");
            if template {
                templates.insert(name.clone(), value.clone());
            }
            if (name.starts_with("E_") || name.starts_with("W_")) && !template {
                codes.insert(name, value);
            }
        }
    }
    assert!(
        codes.len() > 80 && templates.len() > 10,
        "the sources hold the registry: {} codes, {} templates",
        codes.len(),
        templates.len()
    );
    // the registered rows rdy_schema reports, in their order
    let rows: Vec<Value> = rdy_mcp::tools::ERROR_ROWS
        .iter()
        .map(|(code, when, fix)| json!([code, when, fix]))
        .collect();
    // the envelope of an error value
    let e = rdy_engine::RdyError::new(rdy_engine::E_BAD_INPUT, "message", "fix");
    let envelope = shape(&serde_json::to_value(&e).expect("an error serializes"));
    json!({
        "sources": ["crates/*/src error and warning constants", "rdy_mcp::tools::ERROR_ROWS", "rdy_engine::RdyError"],
        "codes": codes,
        "templates": templates,
        "error_rows": rows,
        "envelope": envelope,
        "follower_refusal": follower_refusal(),
        "daemon_build_warning": rdy_mcp::server::DaemonBuildWarning {
            version: "1.2.3".into(),
            daemon_build: "0123456789abcdef".into(),
            client_build: "fedcba9876543210".into(),
        }
        .to_json(),
        "redaction_warning": {
            "copies": rdy_engine::W_REDACTION_COPIES_MESSAGE,
            "inventory": rdy_engine::W_REDACTION_INVENTORY_MESSAGE,
            "quotes": rdy_engine::W_REDACTION_QUOTES_MESSAGE,
            "reuse": rdy_engine::W_REDACTION_REUSE_MESSAGE,
            "quarantine": rdy_engine::W_REDACTION_QUARANTINE_MESSAGE,
            "secret": rdy_engine::W_REDACTION_SECRET_MESSAGE,
        },
    })
}

// ---- config ----

/// relc5: `rdy config` lists every key of the table. One project with nothing set but the
/// defaults, and one with values at each level and a populated `remotes` group, each run as
/// text and as JSON, with and without `--show-origin`. The paths are normalized.
fn config_listing() -> Value {
    let run = |dir: &Path, env: &[(String, String)], paths: &[&Path], args: &[&str]| -> Value {
        let (code, out, err) = rdy_in(dir, env, args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        let text = normalize(&out, paths);
        if args.contains(&"--json") {
            serde_json::from_str(&text).expect("json rows")
        } else {
            json!(text)
        }
    };
    let capture = |dir: &Path, env: &[(String, String)], paths: &[&Path]| -> Value {
        json!({
            "text": run(dir, env, paths, &["config"]),
            "text_show_origin": run(dir, env, paths, &["config", "--show-origin"]),
            "json": run(dir, env, paths, &["--json", "config"]),
            "json_show_origin": run(dir, env, paths, &["--json", "config", "--show-origin"]),
        })
    };
    let (d, r) = project();
    drop(r);
    let (cfg, env) = rdy_qa::fixtures::human_env("human:qa");
    let paths = [d.path(), cfg.path()];
    let defaults = capture(d.path(), &env, &paths);
    let append = |path: PathBuf, text: &str| {
        let mut t = std::fs::read_to_string(&path).unwrap_or_default();
        t.push_str(text);
        std::fs::write(path, t).expect("write config");
    };
    append(d.path().join(".rdy.yaml"), "propose:\n  changes: 7\n");
    append(
        config_levels::local_path(d.path()),
        "signing:\n  sign: always\nremotes:\n  origin: {url: \"https://hub.example.com/team\", mode: share}\n",
    );
    append(
        cfg.path().join("rdy/config.yaml"),
        "views:\n  idle_days: 4\n",
    );
    let configured = capture(d.path(), &env, &paths);
    json!({"defaults": defaults, "configured": configured})
}

fn config_row() -> Value {
    let keys: Vec<Value> = config_levels::KEY_LEVELS
        .iter()
        .map(|k| {
            json!({
                "key": k.key,
                "levels": k.levels.iter().map(|l| serde_json::to_value(l).unwrap()).collect::<Vec<_>>(),
                "type": format!("{:?}", k.ty),
                "default": k.default,
            })
        })
        .collect();
    assert!(!keys.is_empty());
    // an unknown key warns and the project still opens
    let (d, r) = project();
    drop(r);
    let cfg = d.path().join(".rdy.yaml");
    let mut text = std::fs::read_to_string(&cfg).expect("config");
    text.push_str("bogus_key: 1\n");
    std::fs::write(&cfg, &text).expect("write config");
    let warnings: Vec<Value> = config_levels::config_warnings(d.path(), None)
        .iter()
        .map(|w| {
            json!({
                "code": w.code,
                "message": normalize(&w.message, &[d.path()]),
                "fix": normalize(&w.fix, &[d.path()]),
            })
        })
        .collect();
    assert!(
        warnings.iter().any(|w| w["code"] == "W_CONFIG_UNKNOWN"),
        "an unknown key warns: {warnings:?}"
    );
    // a doc file a person edits refuses an unknown key
    let doc = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        doc.path().join("page.yaml"),
        "- id: d-AAAAA\n  type: doc\n  order: a0\n  fields:\n    title: Page\n    doc_type: page\n  bogus: 1\n",
    )
    .expect("write a doc");
    let (cfgdir, env) = rdy_qa::fixtures::human_env("human:qa");
    let (code, out, err) = rdy_in(
        d.path(),
        &env,
        &[
            "--json",
            "check",
            doc.path().join("page.yaml").to_str().unwrap(),
        ],
    );
    drop(cfgdir);
    let body = if out.trim().is_empty() {
        err.trim()
    } else {
        out.trim()
    };
    let refusal: Value = serde_json::from_str(&normalize(body, &[doc.path(), d.path()]))
        .unwrap_or(json!({"text": normalize(body, &[doc.path(), d.path()])}));
    json!({
        "sources": ["rdy_engine::config_levels::KEY_LEVELS", "config_warnings", "rdy check"],
        "project_file": config_levels::PROJECT_FILE,
        "local_file": config_levels::LOCAL_FILE,
        "keys": keys,
        "listing": config_listing(),
        "propose_cadences": config_levels::PROPOSE_CADENCES,
        "sign_modes": config_levels::SIGN_MODES,
        "unknown_key_warning": warnings,
        "editable_file_unknown_key": {"exit": code, "reply": refusal},
    })
}

// ---- schema packs ----

fn packs_row() -> Value {
    let set = PackSet::with_builtin();
    let std_pack = set
        .resolve("rdy-standard")
        .expect("the built-in pack resolves");
    let pack_json = serde_json::to_value(&std_pack).expect("a pack serializes");
    let bad_key = PackSet::default()
        .add_yaml("name: x\nversion: \"1\"\nbogus: 1\n")
        .expect_err("an unknown pack key is refused")
        .to_string();
    let bad_field = PackSet::default()
        .add_yaml(
            "name: x\nversion: \"1\"\nblock_types:\n  t:\n    prefix: t\n    fields:\n      f:\n        kind: nope\n",
        )
        .expect_err("an unknown field kind is refused")
        .to_string();
    let duplicate = {
        let mut s = PackSet::with_builtin();
        s.add_yaml_from("name: rdy-standard\nversion: \"9\"\n", "x.yaml")
            .expect_err("a taken name is refused")
            .to_string()
    };
    // nb3 (D-YXBXH, O6): a pack that sets every coverage key, the level setting's default, and
    // the two refusals at its edges
    let coverage_pack = {
        let mut s = PackSet::with_builtin();
        s.add_yaml(
            "name: probe\nversion: \"1\"\nextends: rdy-standard\ncoverage:\n  weights: {governed: 2}\n  dismissed_weight: 25\n  drift_threshold: 70\n  max_docs_per_level: 12\n",
        )
        .expect("a pack with every coverage key parses");
        s.resolve("probe").expect("it resolves")
    };
    let level_refusal = |n: u32| {
        let mut s = PackSet::with_builtin();
        s.add_yaml(&format!(
            "name: probe\nversion: \"1\"\nextends: rdy-standard\ncoverage:\n  max_docs_per_level: {n}\n"
        ))
        .expect("the key parses");
        s.resolve("probe")
            .expect_err("a level outside 1 to 100000 is refused")
            .to_string()
    };
    let pack_keys: Vec<String> = pack_json.as_object().unwrap().keys().cloned().collect();
    // rtc1 (D-ZSRBN): the policy field `rules_from_chat`: absent reads land, both values, and the
    // two refusals of a value outside the field
    let policy_probe = |value: Option<rdy_model::FieldValue>| -> Value {
        let mut block = rdy_model::Block {
            id: BlockId::parse("ap-APQ01").expect("a policy id"),
            block_type: rdy_schema::policy::POLICY_TYPE.to_string(),
            parent: Some(BlockId::parse("d-D0C11").expect("a doc id")),
            order: "V".to_string(),
            fields: rdy_schema::policy::template_fields(),
            body: String::new(),
            links: Default::default(),
            pins: Default::default(),
            foreign: Default::default(),
            read_identity: None,
        };
        if let Some(v) = value {
            block.fields.insert("rules_from_chat".to_string(), v);
        }
        let mut doc = block.clone();
        doc.id = BlockId::parse("d-D0C11").expect("a doc id");
        doc.block_type = "doc".to_string();
        doc.parent = None;
        doc.fields = [("name", "plan"), ("title", "Plan"), ("doc_type", "page")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), rdy_model::FieldValue::Str(v.to_string())))
            .collect();
        let issues: Vec<Value> = rdy_schema::validate_block(&std_pack, &block, Some(&doc))
            .into_iter()
            .map(|i| json!([i.code, i.message, i.fix]))
            .collect();
        let read = rdy_schema::policy::policy_fields(&std_pack, &block)
            .map(|f| f.rules_from_chat.as_str());
        json!({"issues": issues, "reads": read})
    };
    let text = |s: &str| Some(rdy_model::FieldValue::Str(s.to_string()));
    json!({
        "sources": ["rdy_schema::Pack", "rdy_schema::PackSet", "crates/rdy-schema/packs"],
        "manifest_keys": pack_keys,
        "builtin_packs": [{"name": std_pack.name, "version": std_pack.version}],
        "rdy_standard": pack_json,
        "tension_kinds": TensionKind::ALL.iter().map(|k| k.name()).collect::<Vec<_>>(),
        "tension_table": std_pack
            .tension_table()
            .iter()
            .map(|(k, s, soft)| json!([k.name(), s.to_string(), soft]))
            .collect::<Vec<_>>(),
        "severities": [TensionSeverity::Blocking.to_string(), TensionSeverity::Advisory.to_string()],
        "store_only": {
            "link_kinds": rdy_schema::STORE_ONLY_LINK_KINDS,
            "target_types": rdy_schema::STORE_ONLY_TARGET_TYPES,
            "fields": rdy_schema::STORE_ONLY_FIELDS,
        },
        "limits": {
            "max_reflects_globs": rdy_schema::MAX_REFLECTS_GLOBS,
            "max_glob_len": rdy_schema::MAX_GLOB_LEN,
            "max_path_segments": rdy_schema::MAX_PATH_SEGMENTS,
        },
        "coverage_settings": {
            "pack": coverage_pack.coverage,
            "max_docs_per_level_default": std_pack.max_docs_per_level(),
            "max_docs_per_level_set": coverage_pack.max_docs_per_level(),
        },
        "validation": {
            "unknown_pack_key": bad_key,
            "unknown_field_kind": bad_field,
            "duplicate_pack_name": duplicate,
            "coverage_level_too_low": level_refusal(0),
            "coverage_level_too_high": level_refusal(100_001),
            "policy_rules_from_chat_default": policy_probe(None),
            "policy_rules_from_chat_land": policy_probe(text("land")),
            "policy_rules_from_chat_card": policy_probe(text("card")),
            "policy_rules_from_chat_bad_value": policy_probe(text("later")),
            "policy_rules_from_chat_bad_kind": policy_probe(Some(rdy_model::FieldValue::Int(3))),
        },
    })
}

// ---- markdown exports ----

fn markdown_row() -> Value {
    use rdy_render::print::md::{Layout, write_markdown};
    // fixed ids and a pinned clock: the ids and hashes in the exported text are pinned with it
    rdy_model::seed_ids_for_tests(14);
    let _clock = rdy_qa::golden::fixture_clock();
    let (_d, r, _ids) = rdy_qa::golden::site_with_decision();
    let p = rdy_render::print::build(&r.read(), &Scope::Canon).expect("the print builds");
    let mut layouts = Map::new();
    let heading = Regex::new(r"(?m)^#{1,6} .*$").unwrap();
    let link = Regex::new(r"\]\(([^)\s]+)\)").unwrap();
    let anchor = Regex::new(r#"\{#[^}]+\}|<a (?:id|name)="[^"]+""#).unwrap();
    for (name, layout) in [
        ("plain", Layout::Plain),
        ("mkdocs", Layout::Mkdocs),
        ("techdocs", Layout::Techdocs),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("md");
        write_markdown(&p, &out, layout).expect("write the tree");
        let mut files = Map::new();
        fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for e in std::fs::read_dir(dir).expect("read dir") {
                let p = e.expect("entry").path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    out.push((
                        p.strip_prefix(base)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                        std::fs::read(&p).expect("read"),
                    ));
                }
            }
        }
        let mut all = vec![];
        walk(&out, &out, &mut all);
        all.sort();
        for (rel, bytes) in all {
            let entry = if rel.ends_with(".md") || rel.ends_with(".yml") {
                let text = normalize(&String::from_utf8_lossy(&bytes), &[dir.path()]);
                let front: Value = text
                    .strip_prefix("---\n")
                    .and_then(|r| r.split_once("\n---\n"))
                    .and_then(|(f, _)| serde_yaml_ng::from_str::<Value>(f).ok())
                    .unwrap_or(json!(null));
                json!({
                    "front_matter_keys": front.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()),
                    "headings": heading.find_iter(&text).map(|m| m.as_str().to_string()).collect::<Vec<_>>(),
                    "anchors": anchor.find_iter(&text).map(|m| m.as_str().to_string()).collect::<Vec<_>>(),
                    "link_targets": link.captures_iter(&text).map(|c| c[1].to_string()).collect::<Vec<_>>(),
                    "blake3": blake3_of(text.as_bytes()),
                })
            } else {
                json!({"len": bytes.len(), "blake3": blake3_of(&bytes)})
            };
            files.insert(rel, entry);
        }
        assert!(files.contains_key("index.md") || files.contains_key("docs/index.md"));
        layouts.insert(name.into(), json!({"files": files}));
    }
    rdy_model::unseed_ids_for_tests();
    json!({
        "sources": ["rdy_render::print::md::write_markdown", "rdy_qa::golden site_with_decision"],
        "layouts": layouts,
    })
}

// ---- site urls ----

fn site_row() -> Value {
    use rdy_engine::site_scope::{CHANGES_PAGE, SINCE_PAGE, ScopeRef};
    use rdy_engine::site_target::SiteTarget;
    let hash = Hash([0xab; 32]);
    let proposal = rdy_engine::site_scope::proposal_ref(&format!("P-{}@2", &hash.to_string()[..8]))
        .expect("a proposal reference");
    let scopes = vec![
        ("canon", ScopeRef::Canon),
        ("view", ScopeRef::View("editing".into())),
        ("view_with_slash", ScopeRef::View("team/a b".into())),
        ("proposal", ScopeRef::Proposal(proposal)),
        ("tag", ScopeRef::Tag("v1.2.0".into())),
        ("versions", ScopeRef::Versions),
    ];
    let mut scope_rows = Map::new();
    for (name, s) in &scopes {
        scope_rows.insert(
            (*name).to_string(),
            json!({"text": s.text(), "prefix": s.path_prefix()}),
        );
    }
    let mut targets = Map::new();
    for (name, s) in &scopes {
        let source = SiteTarget::Source(s.clone());
        targets.insert(
            format!("source_{name}"),
            json!({"root": source.root(), "menu": source.menu_url(), "events": source.events_query()}),
        );
    }
    let profile = SiteTarget::Profile {
        scope: ScopeRef::Tag("v1.2.0".into()),
        key: "acme.docs".into(),
    };
    let only = SiteTarget::ProfileOnly { key: "acme".into() };
    let menu = SiteTarget::Menu {
        scope: ScopeRef::View("editing".into()),
        profile: Some("acme".into()),
    };
    for (name, t) in [
        ("profile", &profile),
        ("profile_only", &only),
        ("menu", &menu),
    ] {
        targets.insert(
            name.into(),
            json!({"root": t.root(), "menu": t.menu_url(), "events": t.events_query()}),
        );
    }
    // the pages of a rendered site: canon pages, block permalinks and assets
    rdy_model::seed_ids_for_tests(14);
    let _clock = rdy_qa::golden::fixture_clock();
    let (_d, r, _ids) = rdy_qa::golden::site_with_decision();
    let out = tempfile::tempdir().expect("tempdir");
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).expect("render canon");
    let mut routes: Vec<String> = rdy_qa::golden::pages(out.path())
        .iter()
        .map(|p| {
            normalize(
                &p.strip_prefix(out.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
                &[],
            )
        })
        .collect();
    routes.sort();
    routes.dedup();
    // the asset route: the one top directory of the rendered site that holds its stylesheets
    let asset_route = asset_route_of(out.path());
    rdy_model::unseed_ids_for_tests();
    let has_go = routes
        .iter()
        .any(|r| r.starts_with("go/") && r.ends_with(".html"));
    assert!(has_go, "a block permalink route is in the site: {routes:?}");
    let tag_ok: Vec<Value> = ["v1.2.0", "1.0", ".", "..", "a/b"]
        .iter()
        .map(|t| json!([t, rdy_engine::site_scope::tag_name_ok(t)]))
        .collect();
    let view_ok: Vec<Value> = ["editing", "a:b", ""]
        .iter()
        .map(|t| json!([t, rdy_engine::site_scope::view_name_ok(t)]))
        .collect();
    json!({
        "sources": ["rdy_engine::site_scope", "rdy_engine::site_target", "rdy_render::render_site"],
        "scopes": scope_rows,
        "targets": targets,
        "summary_pages": {"changes": CHANGES_PAGE, "since": SINCE_PAGE},
        "tag_name_ok": tag_ok,
        "view_name_ok": view_ok,
        "percent_encode": rdy_engine::site_scope::percent_encode("a b/c.d"),
        "canon_routes": routes,
        "block_route": "go/<block id>.html",
        "asset_route": asset_route,
    })
}

/// The route of a rendered site's assets: the top directory of `site` that holds every
/// stylesheet and script, with a trailing slash. A site with none, or with them in two places, is
/// no contract row.
fn asset_route_of(site: &Path) -> String {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).expect("read the rendered site") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                walk(&p, base, out);
            } else if p.extension().is_some_and(|x| x == "css" || x == "js") {
                let rel = p.strip_prefix(base).expect("under the site");
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut files = vec![];
    walk(site, site, &mut files);
    let mut tops: Vec<String> = files
        .iter()
        .map(|f| match f.split_once('/') {
            Some((top, _)) => format!("{top}/"),
            None => String::new(),
        })
        .collect();
    tops.sort();
    tops.dedup();
    assert!(
        tops.len() == 1 && !tops[0].is_empty(),
        "one asset directory holds the stylesheets and scripts: {files:?}"
    );
    tops.remove(0)
}

/// The sync batch purposes: each header byte the reader accepts, with the purpose's JSON name.
fn purposes() -> Value {
    let rows: Vec<Value> = (0..=u8::MAX)
        .filter_map(|b| sync_stream::StreamPurpose::of(b).map(|p| (b, p)))
        .map(|(b, p)| {
            assert_eq!(p.byte(), b, "a purpose's byte reads back as itself");
            json!([b, p])
        })
        .collect();
    assert!(!rows.is_empty(), "the reader accepts a purpose");
    json!(rows)
}

// ---- derived identities ----

fn identities_row() -> Value {
    let block = |s: &str| BlockId::parse(s).expect("a block id");
    let h = |n: u8| Hash([n; 32]);
    let involved = vec![
        Involved {
            side: SideRef::Change(h(1)),
            block: block("c-AAAAA"),
            version: Some(h(2)),
            content: Some(h(3)),
        },
        Involved {
            side: SideRef::Canon {
                landing: Some(h(4)),
            },
            block: block("c-AAAAA"),
            version: Some(h(5)),
            content: Some(h(6)),
        },
    ];
    let id = tension_id(
        TensionKind::EditCollision,
        TensionSeverity::Blocking,
        &involved,
    );
    let mut reversed = involved.clone();
    reversed.reverse();
    assert_eq!(
        id,
        tension_id(
            TensionKind::EditCollision,
            TensionSeverity::Blocking,
            &reversed
        ),
        "the order of the sides does not matter"
    );
    let vkey = tension_vkey(
        TensionKind::EditCollision,
        TensionSeverity::Blocking,
        &involved,
    );
    let items = [
        PairItem {
            block: block("c-AAAAA"),
            content: h(7),
        },
        PairItem {
            block: block("c-BBBBB"),
            content: h(8),
        },
    ];
    let side = |change: u8, version: u8| rdy_store::merge::Side {
        change: Some(h(change)),
        version: Some(h(version)),
    };
    let conflict =
        rdy_store::merge::conflict_id(&block("c-AAAAA"), Some(h(9)), &[side(1, 2), side(3, 4)]);
    // the scenario: derived state of a project, before and after a reindex, from fixed ids and
    // a pinned clock
    rdy_model::seed_ids_for_tests(14);
    let _clock = rdy_qa::golden::fixture_clock();
    let (d, r) = rdy_qa::packs::qa_project();
    let e = rdy_engine::Local::new(r);
    rdy_qa::inbox::build_agent(&e);
    let before = rdy_qa::inbox::on(&e, |r| rdy_qa::reindex_check::derived(r).expect("derived"));
    rdy_qa::inbox::on(&e, |r| r.reindex().expect("reindex"));
    let after = rdy_qa::inbox::on(&e, |r| rdy_qa::reindex_check::derived(r).expect("derived"));
    rdy_model::unseed_ids_for_tests();
    drop(d);
    assert_eq!(before, after, "a reindex preserves the derived identities");
    let consequence =
        rdy_engine::consequence::consequence_id(&h(1), "premise_shift", &block("c-AAAAA"));
    json!({
        "sources": ["rdy_engine::tensions", "rdy_store::merge", "rdy_model::semantic", "rdy_engine::consequence", "rdy_qa::reindex_check"],
        "domains": {
            "tension": "rdy-tension-2",
            "tension_vkey": "rdy-tension-vkey-2",
            "pair": "rdy-pair-1",
            "consequence": "rdy-consequence",
            "case": "rdy-case-1",
            "content": "rdy-content-2",
        },
        "tension": {
            "id": id.to_string(),
            "vkey": vkey.to_string(),
            "label": tension_label(&id),
        },
        "pair_key": pair_key(&items).to_string(),
        "conflict_id": conflict.to_string(),
        "consequence_id": consequence,
        "scenario": derived_json(&after),
    })
}

fn derived_json(d: &rdy_qa::reindex_check::Derived) -> Value {
    json!({
        "tensions": d.tensions,
        "conflicts": d.conflicts,
        "inboxes": d.inboxes,
        "logged_tensions": d.logged_tensions,
        "logged_suspects": d.logged_suspects,
        "transitions": d.transitions,
        "events": d.events,
    })
}

// ---- the inventory ----

fn generate() -> BTreeMap<&'static str, Value> {
    let mut rows = BTreeMap::new();
    rows.insert("store", store_row());
    rows.insert("dump_load", dump_load_row());
    rows.insert("sync", sync_row());
    rows.insert("working_copy", wc_row());
    rows.insert("signatures", signatures_row());
    rows.insert("mcp", mcp_row());
    rows.insert("errors_warnings", errors_row());
    rows.insert("config", config_row());
    rows.insert("schema_packs", packs_row());
    rows.insert("markdown_exports", markdown_row());
    rows.insert("site_urls", site_row());
    rows.insert("derived_identities", identities_row());
    // the cli row is generated here only for its runtime member
    rows.insert("cli", json!({"runtime": cli_runtime()}));
    rows
}

/// The released baseline, `release-1.json`, frozen at the Release 1 tag. The guards that pin
/// the bytes of released sections read it; every live comparison reads [`read_fixture`].
fn read_released() -> Value {
    let path = qa_root().join("fixtures/contracts/release-1.json");
    let text = std::fs::read_to_string(&path).expect("the released contract baseline");
    serde_json::from_str(&text).expect("the released contract baseline is JSON")
}

fn read_fixture() -> Value {
    if capture_mode() {
        // the generation writes the baseline once, before any test reads it
        rows();
    }
    let path = fixture_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "the release contract baseline {} is absent ({e}); capture it with RDY_CONTRACT_CAPTURE=1",
            path.display()
        )
    });
    serde_json::from_str(&text).expect("the release contract fixture is JSON")
}

fn capture(rows: &BTreeMap<&'static str, Value>) {
    let mut doc: Value = std::fs::read_to_string(fixture_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    doc["inventory_version"] = json!(INVENTORY_VERSION);
    for (name, row) in rows {
        if *name == "cli" {
            if !doc["cli"].is_object() {
                doc["cli"] = json!({});
            }
            doc["cli"]["runtime"] = row["runtime"].clone();
        } else {
            doc[*name] = row.clone();
        }
    }
    std::fs::create_dir_all(fixture_path().parent().unwrap()).expect("create the directory");
    let text = serde_json::to_string_pretty(&doc).expect("encode") + "\n";
    std::fs::write(fixture_path(), text).expect("write the fixture");
}

/// A cached generation: the rows take a while, and three tests read them.
fn rows() -> &'static BTreeMap<&'static str, Value> {
    static ROWS_NOW: std::sync::OnceLock<BTreeMap<&'static str, Value>> =
        std::sync::OnceLock::new();
    ROWS_NOW.get_or_init(|| {
        let rows = generate();
        if capture_mode() {
            capture(&rows);
        }
        rows
    })
}

#[test]
fn ok14_release_contract_matches_all_surfaces() {
    let fixture = read_fixture();
    assert_eq!(fixture["inventory_version"], json!(INVENTORY_VERSION));
    let live = rows();
    for name in ROWS {
        let now = live
            .get(name)
            .unwrap_or_else(|| panic!("no generator for {name}"));
        let want = &fixture[name];
        if name == "cli" {
            assert!(
                want["tree"].is_object(),
                "the fixture's cli.tree is the command tree rdy-cli compares"
            );
            assert_eq!(
                &now["runtime"], &want["runtime"],
                "row {name}: the release binary's exit values or JSON fields changed"
            );
            continue;
        }
        assert!(want.is_object(), "the fixture has no row {name}");
        assert_eq!(
            now, want,
            "row {name} changed: a public contract change needs an upgrade or deprecation path (D-TXFHR)"
        );
    }
}

/// Every member typed by hand is listed in [`DESCRIPTIVE`], and each listed member is in the
/// fixture, so the list says exactly which members cannot fail on their own.
#[test]
fn ok14_descriptive_members_are_listed_and_present() {
    let fixture = read_fixture();
    let mut sorted = DESCRIPTIVE.to_vec();
    sorted.sort();
    assert_eq!(sorted, DESCRIPTIVE, "sorted, no repeat");
    sorted.dedup();
    assert_eq!(sorted.len(), DESCRIPTIVE.len());
    for path in DESCRIPTIVE {
        let mut v = &fixture;
        for part in path.split('.') {
            v = v
                .get(part)
                .unwrap_or_else(|| panic!("{path}: the fixture has no member {part}"));
        }
        assert!(!v.is_null(), "{path}");
    }
}

#[test]
fn ok14_contract_inventory_cannot_drop_a_surface() {
    let fixture = read_fixture();
    let mut have: Vec<String> = fixture
        .as_object()
        .expect("the inventory is an object")
        .keys()
        .filter(|k| *k != "inventory_version")
        .cloned()
        .collect();
    have.sort();
    assert_eq!(
        have,
        ROWS.to_vec(),
        "the inventory holds the 13 surface rows, no more and no fewer"
    );
    // a row with no content is not coverage
    for name in ROWS {
        let row = &fixture[name];
        let members = row
            .as_object()
            .unwrap_or_else(|| panic!("row {name} is not an object"))
            .len();
        assert!(members >= 2, "row {name} holds no facts");
        if name != "cli" {
            assert!(
                row["sources"].as_array().is_some_and(|s| !s.is_empty()),
                "row {name} names no source"
            );
        }
    }
    // every generator is wired: the live generation has the same 13 rows
    let mut live: Vec<&str> = rows().keys().copied().collect();
    live.sort();
    assert_eq!(live, ROWS.to_vec());
}

#[test]
fn ok14_reindex_preserves_release_derived_identities() {
    let fixture = read_fixture();
    let want = &fixture["derived_identities"];
    let now = &rows()["derived_identities"];
    // the identities of fixed inputs and of the seeded scenario, before and after a rebuild:
    // generation asserted equality; the fixture pins the values
    assert_eq!(
        &now["scenario"], &want["scenario"],
        "the derived identities of unchanged input moved"
    );
    assert_eq!(&now["tension"], &want["tension"]);
    assert_eq!(&now["pair_key"], &want["pair_key"]);
    assert_eq!(&now["conflict_id"], &want["conflict_id"]);
    assert_eq!(&now["consequence_id"], &want["consequence_id"]);
    let scenario = &want["scenario"];
    assert!(
        scenario["tensions"]
            .as_object()
            .is_some_and(|m| !m.is_empty()),
        "the scenario holds a tension: {scenario}"
    );
    assert!(
        scenario["conflicts"]
            .as_object()
            .is_some_and(|m| !m.is_empty()),
        "the scenario holds a conflict: {scenario}"
    );
    assert!(
        scenario["inboxes"]
            .as_object()
            .is_some_and(|m| !m.is_empty()),
        "the scenario holds an inbox: {scenario}"
    );
}

/// No tool or command names an assessment. ap5 (D-ZWP7Q "Disputes") builds the dispute writers,
/// and M3d (D-JQD5X) enables the whole check family: CHECK_REQUEST, the Check review with its
/// three verdicts, and AcceptVerdict keep the kind, encoding and enum positions the store
/// carried.
#[test]
fn ck6_m3d_carriers_keep_their_wire_shapes() {
    let fixture = read_fixture();
    // the MCP tools: no name or parameter writes an assessment
    let tools = fixture["mcp"]["tools"]
        .as_object()
        .expect("the MCP tools are pinned");
    for name in tools.keys() {
        assert!(
            !name.to_lowercase().contains("assessment"),
            "tool {name} names a future record: assessment"
        );
    }
    // the CLI tree: no command or flag names an assessment
    let mut seen = vec![];
    fn names(t: &Value, out: &mut Vec<String>) {
        out.push(t["name"].as_str().unwrap_or_default().to_string());
        for a in t["args"].as_array().into_iter().flatten() {
            out.push(a["id"].as_str().unwrap_or_default().to_string());
            for c in a["choices"].as_array().into_iter().flatten() {
                out.push(c.as_str().unwrap_or_default().to_string());
            }
        }
        for s in t["subcommands"].as_array().into_iter().flatten() {
            names(s, out);
        }
    }
    names(&fixture["cli"]["tree"], &mut seen);
    assert!(seen.len() > 100, "the CLI tree is pinned");
    for n in &seen {
        assert!(
            !n.to_lowercase().contains("assessment"),
            "the CLI names a future record: {n}"
        );
    }
    // the check family keeps its wire shapes: kind 28 at encoding 1, and the appended enum
    // positions of the verdicts, the Check subject, the act and the request mode
    assert_eq!(kinds::CHECK_REQUEST, 28);
    assert!(codec::REGISTERED.contains(&(kinds::CHECK_REQUEST, 1)));
    assert_eq!(codec::first(kinds::CHECK_REQUEST), 1);
    let index = |bytes: Vec<u8>| bytes[0];
    for (v, at) in [
        (Verdict::Pass, 4),
        (Verdict::Concern, 5),
        (Verdict::Fail, 6),
    ] {
        assert_eq!(index(postcard::to_allocvec(&v).unwrap()), at, "{v:?}");
    }
    let check = ReviewSubject::Check {
        request: Hash([0; 32]),
    };
    assert_eq!(index(postcard::to_allocvec(&check).unwrap()), 3);
    assert_eq!(
        index(postcard::to_allocvec(&AnswerAct::AcceptVerdict).unwrap()),
        16
    );
    use rdy_model::check_request::CheckMode;
    assert_eq!(index(postcard::to_allocvec(&CheckMode::Author).unwrap()), 0);
    assert_eq!(
        index(postcard::to_allocvec(&CheckMode::Independent).unwrap()),
        1
    );
    // a fabricated request is no request: the review writer reads it as stale, whatever its
    // verdict; and `check` is no check
    let refusals = fixture["mcp"]["carried_value_refusals"]
        .as_object()
        .expect("the refusals are pinned");
    for v in ["pass", "concern", "fail"] {
        assert_eq!(
            refusals[&format!("rdy_review_submit {v}")],
            json!({
                "code": "E_REVIEW_STALE",
                "message": "request rq-000000000000 is no longer current: a side changed, landed or left every view",
                "fix": "call rdy_review_next again",
            }),
            "the fabricated request with verdict {v}"
        );
    }
    assert_eq!(
        refusals["rdy_review_next check"],
        json!({
            "code": "E_BAD_INPUT",
            "message": "check is not a check",
            "fix": "list the checks: rdy_review {op: \"checks\"} / rdy review checks",
        }),
        "check is an unknown check"
    );
}

/// ck8 (D-JQD5X): the contract holds the check surfaces with populated replies: the tool and its
/// closed schema, the catalogue, a run, a request, the list with both verdicts, the two verdict
/// replies, and a `change` reply whose `on_change.checks` holds a failure row. Every field of
/// the earlier replies stays.
#[test]
fn ck8_wire_contract_adds_check_surfaces() {
    let fixture = read_fixture();
    let mcp = &fixture["mcp"];
    // the tool is listed, and its schema is closed: `op` is required, the rest optional
    assert!(
        mcp["tool_names"]
            .as_array()
            .unwrap()
            .contains(&json!("rdy_review"))
    );
    let schema = &mcp["tools"]["rdy_review"]["input_schema"];
    assert_eq!(schema["additionalProperties"], json!(false));
    assert_eq!(schema["required"], json!(["op"]));
    let mut props: Vec<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    props.sort_unstable();
    assert_eq!(
        props,
        [
            "check",
            "evidence",
            "op",
            "open",
            "question",
            "reason",
            "session_ref",
            "subject"
        ]
    );
    let r = &mcp["result_fields"];
    // the catalogue
    assert_eq!(
        r["rdy_review checks"]["ok"],
        json!({"checks": [{"name": "string", "subject": "string", "verdicts": ["string"]}]})
    );
    // a run and a request: the first request, its case and every request
    for label in ["rdy_review run", "rdy_review request"] {
        let ok = &r[label]["ok"];
        for key in ["request", "case_hash"] {
            assert_eq!(ok[key], json!("string"), "{label} {key}");
        }
        assert_eq!(ok["omitted"], json!("number"), "{label}");
        assert_eq!(ok["case"]["kind"], json!("string"), "{label}");
        assert_eq!(
            ok["requests"][0]["request"],
            json!("string"),
            "{label} requests"
        );
        assert_eq!(ok["requests"][0]["case_hash"], json!("string"), "{label}");
        assert!(ok["requests"][0]["case"].is_object(), "{label}");
    }
    // the list: a row holds every key, with a populated verdict
    let row = &r["rdy_review list"]["ok"]["requests"][0];
    for key in [
        "request",
        "check",
        "scope",
        "mode",
        "case_hash",
        "status",
        "reason",
    ] {
        assert!(row[key].is_string() || row[key].is_object(), "{key}: {row}");
    }
    assert_eq!(row["subject"]["blocks"], json!(["string"]));
    assert_eq!(row["requested_by"]["actor"], json!("string"));
    let verdict = &row["verdicts"][0];
    for key in ["review", "verdict", "actor", "run", "case_hash"] {
        assert!(verdict[key].is_string(), "{key}: {verdict}");
    }
    // the pass has no reason and the concern has one
    assert_eq!(verdict["reason"], json!("null|string"), "{verdict}");
    assert_eq!(verdict["citations"][0]["block"], json!("string"));
    assert_eq!(verdict["citations"][0]["note"], json!("string"));
    assert_eq!(r["rdy_review list"]["ok"]["omitted"], json!("number"));
    // the two verdict replies and the claim of an independent request
    for label in ["rdy_review_submit concern", "rdy_review_submit pass"] {
        assert_eq!(
            r[label]["ok"],
            json!({"request": "string", "review": "string", "verdict": "string"}),
            "{label}"
        );
    }
    let next = &r["rdy_review_next decision_review"]["ok"];
    assert_eq!(next["kind"], json!("string"));
    assert!(next["case"]["blocks"][0]["id"].is_string());
    // the change reply: a failure row with every key
    assert_eq!(
        r["rdy_write"]["ok"]["on_change"]["checks"],
        json!([{"rule": "string", "code": "string", "block": "string", "message": "string", "fix": "string"}])
    );
    // every earlier field stays
    for (tool, fields) in [
        (
            "rdy_review_next",
            &["case", "case_hash", "kind", "lease_expires_at", "request"][..],
        ),
        ("rdy_review_submit", &["request", "review", "verdict"][..]),
        (
            "rdy_write",
            &["change", "consequences", "coverage", "created", "on_change"][..],
        ),
    ] {
        for field in fields {
            assert!(
                !r[tool]["ok"][*field].is_null(),
                "{tool} lost its field {field}"
            );
        }
    }
    for field in ["resolved", "tensions"] {
        assert!(
            !r["rdy_write"]["ok"]["on_change"][field].is_null(),
            "on_change lost {field}"
        );
    }
    assert!(
        r["rdy_review_submit"]["refusal_codes"]
            .as_array()
            .is_some_and(|c| !c.is_empty()),
        "the tension verdict keeps its refusals"
    );
}

/// The top-level commands of the contract before ck9, in order: every one stays.
const COMMANDS_BEFORE_CK9: [&str; 64] = [
    "init",
    "status",
    "show",
    "log",
    "impact",
    "trace",
    "brief",
    "search",
    "diff",
    "q",
    "suspects",
    "plan",
    "reaffirm",
    "repin",
    "propose",
    "redact",
    "approve",
    "land",
    "dispute",
    "policy",
    "view",
    "alt",
    "intent",
    "tensions",
    "resolve",
    "inbox",
    "pending",
    "answer",
    "coverage",
    "undo",
    "tag",
    "tags",
    "new",
    "publication",
    "publish",
    "ls",
    "edit",
    "site",
    "render",
    "export",
    "import",
    "dump",
    "load",
    "clone",
    "reindex",
    "fsck",
    "upgrade",
    "mcp",
    "check",
    "pack",
    "keys",
    "sign",
    "key",
    "render-stream",
    "stream",
    "serve",
    "config",
    "remote",
    "auth",
    "checkout",
    "add",
    "commit",
    "update",
    "worktree",
];

/// The ids of a command's own arguments: those the global flags and `help` do not make.
fn own_args(cmd: &Value) -> Vec<String> {
    cmd["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["global"] != json!(true) && a["id"] != "help")
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect()
}

/// The keys of a shape object, sorted, with the mark of an optional key left off.
fn shape_keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v
        .as_object()
        .unwrap_or_else(|| panic!("an object shape: {v}"))
        .keys()
        .map(|k| k.trim_end_matches('?').to_string())
        .collect();
    k.sort();
    k
}

/// ck9 (D-JQD5X, D-WMCFW): the contract holds the command line of the checks and the complete
/// approval loop with populated replies: the `review` tree with its closed grammar, the four
/// commands' replies (a run, a request, the catalogue and a list with both verdicts), the person's
/// card with its verdict rows and acceptance option, the AcceptVerdict answer, and the refusal
/// and the queue row that the accepted verdict makes. Every earlier command and reply key stays.
#[test]
fn ck9_release_probes_cover_nonempty_checks_and_acceptance() {
    let fixture = read_fixture();
    let tree = &fixture["cli"]["tree"];
    let top: Vec<&str> = tree["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    // the earlier commands, in order, and the new one
    // (mh7 added `compact` after them: its own pin is `mh7_new_surfaces_preserve_the_release_contract`;
    // ain5 added `instructions` and `setup`, and rtc6 added `rule`: their pin is `ok14_cli_contract_from_clap_is_complete`)
    let earlier: Vec<&str> = top
        .iter()
        .copied()
        .filter(|n| !["review", "compact", "instructions", "setup", "rule"].contains(n))
        .collect();
    assert_eq!(earlier, COMMANDS_BEFORE_CK9);
    let review = tree["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "review")
        .expect("the review command");
    assert_eq!(review["subcommand_required"], json!(true));
    let subs: Vec<&str> = review["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        subs,
        ["checks", "run", "request", "list"],
        "no submit and no accept"
    );
    let sub = |name: &str| {
        review["subcommands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap()
    };
    assert!(own_args(sub("checks")).is_empty());
    // one subject, or two values: the third is clap's refusal
    for name in ["run", "request"] {
        let positional: Vec<(&str, u64, bool)> = sub(name)["args"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["positional"] == json!(true))
            .map(|a| {
                (
                    a["id"].as_str().unwrap(),
                    a["index"].as_u64().unwrap(),
                    a["required"] == json!(true),
                )
            })
            .collect();
        assert_eq!(
            positional,
            [
                ("check", 1, true),
                ("subject", 2, true),
                ("second", 3, false)
            ],
            "{name}"
        );
    }
    assert_eq!(own_args(sub("run")), ["check", "subject", "second"]);
    // the relay flags are the answer's, with no run: the run comes from the session
    assert_eq!(
        own_args(sub("request")),
        [
            "check",
            "subject",
            "second",
            "reason",
            "relayed_by",
            "evidence",
            "question",
            "session_ref"
        ]
    );
    let longs: Vec<&str> = sub("request")["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["long"].as_str())
        .collect();
    for flag in ["reason", "relayed-by", "evidence", "question", "session"] {
        assert!(longs.contains(&flag), "{flag}: {longs:?}");
    }
    assert!(longs.iter().all(|l| !l.contains("run")), "{longs:?}");
    assert_eq!(own_args(sub("list")), ["subject", "open"]);

    // the replies, populated
    let probes = &fixture["cli"]["runtime"]["probes"];
    let stdout = |label: &str| &probes[label]["stdout"];
    assert_eq!(
        stdout("review checks"),
        &json!({"checks": [{"name": "string", "subject": "string", "verdicts": ["string"]}]})
    );
    for label in ["review run", "review request"] {
        let ok = stdout(label);
        assert_eq!(
            shape_keys(ok),
            ["case", "case_hash", "omitted", "request", "requests"]
        );
        assert_eq!(ok["case"]["kind"], json!("string"), "{label}");
        assert_eq!(
            shape_keys(&ok["requests"][0]),
            ["case", "case_hash", "request"],
            "{label}"
        );
        assert_eq!(probes[label]["exits"], json!([0]), "{label}");
    }
    let row = &stdout("review list")["requests"][0];
    assert_eq!(
        shape_keys(row),
        [
            "case_hash",
            "check",
            "mode",
            "reason",
            "request",
            "requested_by",
            "scope",
            "status",
            "subject",
            "verdicts"
        ]
    );
    assert_eq!(
        shape_keys(&row["verdicts"][0]),
        [
            "actor",
            "case_hash",
            "citations",
            "reason",
            "review",
            "run",
            "verdict"
        ]
    );
    assert_eq!(
        row["verdicts"][0]["citations"][0],
        json!({"block": "string", "note": "string"})
    );
    assert_eq!(
        row["verdicts"][0]["reason"],
        json!("null|string"),
        "a pass and a concern"
    );

    // the person's card: the verdict rows after the stored facts, and the options of the card
    let item = &stdout("pending")["items"][0];
    let rows = &item["check_verdicts?"];
    assert_eq!(
        shape_keys(&rows[0]),
        [
            "actor",
            "case_hash",
            "check",
            "citations",
            "mode",
            "reason",
            "request",
            "review",
            "run",
            "verdict"
        ]
    );
    assert_eq!(
        rows[0]["citations"][0],
        json!({"block": "string", "note": "string"})
    );
    assert_eq!(
        shape_keys(&item["options"][0]),
        ["consequences", "id", "label", "reason", "recommended"]
    );
    // the AcceptVerdict answer, and what it makes: a refusal with issues and a queue row
    let accepted = stdout("answer accept_verdict");
    assert_eq!(
        shape_keys(accepted),
        [
            "act",
            "answer",
            "assurance",
            "label",
            "landing",
            "next",
            "option",
            "ref",
            "relayed_by"
        ]
    );
    assert_eq!(probes["answer accept_verdict"]["exits"], json!([0]));
    assert_eq!(probes["approve held"]["exits"], json!([1]));
    assert_eq!(
        probes["approve held"]["stderr"]["issues"][0],
        json!({"block": "null", "code": "string", "fix": "string", "message": "string", "severity": "string"})
    );
    let not_ready = &stdout("pending")["not_ready"][0];
    assert_eq!(shape_keys(not_ready), ["issues", "ref"]);
    assert_eq!(not_ready["issues"][0]["message"], json!("string"));

    // every reply key of the earlier probes stays: the queue, its items and options, and the
    // answer, as the contract held them before ck9
    assert_eq!(
        shape_keys(stdout("pending")),
        ["items", "not_ready", "omitted", "snoozed"]
    );
    let old_item = [
        "age",
        "batch",
        "blocking",
        "card",
        "card_hash",
        "exception",
        "facts",
        "kind",
        "members",
        "options",
        "raised_at",
        "raised_by",
        "ref",
        "summary",
        "summary_by",
        "title",
        "why",
    ];
    let have = shape_keys(item);
    for key in old_item {
        assert!(
            have.iter().any(|k| k == key),
            "a pending item lost {key}: {have:?}"
        );
    }
    assert_eq!(
        have.len(),
        old_item.len() + 1,
        "check_verdicts is the one addition: {have:?}"
    );
    let answer = stdout("answer");
    for key in [
        "act",
        "answer",
        "assurance",
        "label",
        "landing",
        "next",
        "option",
        "ref",
        "relayed_by",
    ] {
        assert!(!answer[key].is_null(), "an answer lost {key}");
    }
    assert_eq!(
        shape_keys(&probes["answer"]["stderr"]),
        ["code", "fix", "message"]
    );
}

/// A source with a test hook early in a struct, a test module declaration, and product code
/// after both: a scan that stops at the first `#[cfg(test)]` never sees the lines below them.
const PLANTED: &str = r#"
struct Shared {
    seen: u8,
    #[cfg(test)]
    hook: Option<Box<dyn Fn(u8, u8) -> u8>>,
    other: u8,
}
#[cfg(test)]
mod contract;
fn early() -> char { '{' }
#[cfg(test)]
impl Shared {
    fn probe(&self) { let _ = AnswerAct::Dispute; }
}
pub const E_PLANTED: &str = "E_PLANTED";
fn parse(s: &str) -> Option<Verdict> {
    match s {
        "pass" => Some(Verdict::Pass),
        _ => None,
    }
}
fn two(s: &str) -> Verdict { match s { "a" => Verdict::Pass, "b" => Verdict::Fail, _ => x } }
fn late() -> Answer { Answer::new(AnswerAct::ResolveDispute) }
fn reads(v: Verdict) -> bool { v == Verdict::Fail || matches!(v, Verdict::Concern) }
fn arm(v: Verdict) -> u8 { match v { Verdict::Pass | Verdict::Fail => 1, _ => 0 } }
fn subject(s: &ReviewSubject) -> bool { matches!(s, ReviewSubject::Check { .. }) }
fn build(request: Hash) -> ReviewSubject { ReviewSubject::Check { request } }
#[cfg(test)]
mod tests {
    fn t() { let _ = ReviewSubject::Check { request }; let c = '}'; }
}
const AFTER: &str = "x";
fn stmt_site(x: bool) {
    #[cfg(test)]
    if x {
        let _ = 1;
    } else if !x {
        let _ = 2;
    }
    let v = Verdict::Pass;
    #[cfg(test)]
    thread_local! {
        static HOOK: u8 = const { 0 };
    }
    pub const E_X: &str = "E_X";
    #[cfg(test)]
    match x {
        true => {}
        false => {}
    }
    let w = Verdict::Fail;
}
"#;

#[test]
fn ok14_source_scan_keeps_product_code_after_an_early_test_hook() {
    let product = product_text(PLANTED);
    assert_eq!(
        product.lines().count(),
        PLANTED.lines().count(),
        "lines stay aligned"
    );
    for kept in [
        "other: u8",
        "E_PLANTED",
        "fn late",
        "const AFTER",
        "fn build",
    ] {
        assert!(
            product.contains(kept),
            "product code {kept:?} was dropped:\n{product}"
        );
    }
    for gone in [
        "hook",
        "mod contract",
        "fn probe",
        "mod tests",
        "let c = '}'",
    ] {
        assert!(
            !product.contains(gone),
            "test code {gone:?} was kept:\n{product}"
        );
    }
    // the errors row sees a constant defined after the hook
    let consts = Regex::new(r#"const\s+(E_[A-Z_]+)\s*:"#).unwrap();
    assert!(consts.is_match(&product));
}

#[test]
fn ok14_source_scan_ends_a_test_statement_and_a_test_macro_at_their_own_closer() {
    let product = product_text(PLANTED);
    for kept in [
        "let v = Verdict::Pass;",
        "pub const E_X",
        "let w = Verdict::Fail;",
    ] {
        assert!(
            product.contains(kept),
            "product code {kept:?} after a test statement or macro was dropped:\n{product}"
        );
    }
    for gone in [
        "let _ = 1;",
        "let _ = 2;",
        "thread_local",
        "static HOOK",
        "true => {}",
    ] {
        assert!(
            !product.contains(gone),
            "test code {gone:?} was kept:\n{product}"
        );
    }
    let hits = carried_writes("planted.rs", &product, &["Verdict::Pass", "Verdict::Fail"]);
    let text = hits.join("\n");
    assert!(text.contains("let v = Verdict::Pass;"), "{text}");
    assert!(text.contains("let w = Verdict::Fail;"), "{text}");
}

/// Every real source file under the crates: a blanked test item ends at its own closer. The
/// attribute line sits at an indentation; every later line of the blanked run is indented deeper,
/// except the last, which may start with a closer (`}`, `)`, `]`) at the same indentation. A run
/// that swallowed the next statement or item fails here.
#[test]
fn ok14_source_scan_blanks_no_line_outside_the_item_a_test_attribute_marks() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).expect("read the sources") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut files = vec![];
    for krate in std::fs::read_dir(workspace_root().join("crates")).expect("crates") {
        let src = krate.expect("crate").path().join("src");
        if src.is_dir() {
            walk(&src, &mut files);
        }
    }
    files.sort();
    assert!(files.len() > 50, "the crates have sources");
    let indent = |l: &str| l.len() - l.trim_start().len();
    let mut offenders = vec![];
    let mut checked = 0;
    for f in files {
        let text = std::fs::read_to_string(&f).expect("read a source file");
        if text.starts_with("#![cfg(test)]") {
            continue;
        }
        let lines: Vec<&str> = text.lines().collect();
        // the lines that start inside a string, raw string or block comment: their indentation
        // is text, not code
        let mut inside = std::collections::BTreeSet::new();
        {
            let b = text.as_bytes();
            let (mut i, mut line) = (0, 0usize);
            while i < b.len() {
                if let Some(n) = skip_literal(b, i) {
                    inside.extend(
                        (line + 1..=line + b[i..n].iter().filter(|c| **c == b'\n').count())
                            .collect::<Vec<_>>(),
                    );
                    line += b[i..n].iter().filter(|c| **c == b'\n').count();
                    i = n;
                    continue;
                }
                if b[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
        }
        for (from, to) in test_regions(&text) {
            let first = text[..from].matches('\n').count();
            let last = text[..to].matches('\n').count();
            let attr_line = lines[first];
            if !attr_line.trim_start().starts_with("#[cfg(test)]") {
                continue;
            }
            checked += 1;
            let base = indent(attr_line);
            // the item's own first line follows the attribute (and any further attributes or
            // comments); an attribute written before the item on one line has no such line
            let mut body_from = first + 1;
            if attr_line.trim() == "#[cfg(test)]" {
                while lines.get(body_from).is_some_and(|l| {
                    l.trim_start().starts_with("#[") || l.trim_start().starts_with("//")
                }) {
                    body_from += 1;
                }
                body_from += 1;
            }
            for (n, l) in lines.iter().enumerate().take(last + 1).skip(body_from) {
                if l.trim().is_empty() || indent(l) > base || inside.contains(&n) {
                    continue;
                }
                let closer = l.trim_start().starts_with(['}', ')', ']']);
                if !(closer && indent(l) == base) {
                    offenders.push(format!(
                        "{}:{}: blanked, but outside the item marked at line {}: {}",
                        f.display(),
                        n + 1,
                        first + 1,
                        l.trim()
                    ));
                }
            }
        }
    }
    assert!(checked > 100, "the sources hold test items ({checked})");
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}

#[test]
fn ok14_source_scan_finds_a_writer_after_a_hook_and_in_a_match_arm() {
    let product = product_text(PLANTED);
    let needles = [
        "AnswerAct::Dispute",
        "AnswerAct::ResolveDispute",
        "Verdict::Pass",
        "Verdict::Concern",
        "Verdict::Fail",
        "ReviewSubject::Check",
    ];
    // the old cut at the first attribute sees none of them
    let cut = PLANTED.split("#[cfg(test)]").next().unwrap();
    assert!(carried_writes("planted.rs", cut, &needles).is_empty());
    let hits = carried_writes("planted.rs", &product, &needles);
    let text = hits.join("\n");
    // the parser arm, both arms of the one-line match, the late construction, the builder and the
    // two statements that follow a test `if` and a test macro
    assert_eq!(hits.len(), 7, "{text}");
    for want in [
        "\"pass\" => Some",
        "\"b\" => Verdict::Fail",
        "AnswerAct::ResolveDispute",
        "ReviewSubject::Check { request }",
    ] {
        assert!(text.contains(want), "no hit for {want}:\n{text}");
    }
    // reads and the test module are not hits
    for not in ["fn reads", "fn arm", "fn subject", "fn probe", "fn t()"] {
        assert!(!text.contains(not), "{not} is a read or a test:\n{text}");
    }
}

/// A carried value inside a `matches!(..)` is a read; one after the `matches!` closed, on the
/// same line, is a write.
#[test]
fn ok14_source_scan_counts_a_write_after_a_closed_matches() {
    let needles = ["Verdict::Pass"];
    let read = "fn r(v: Verdict) -> bool { matches!(v, Verdict::Pass) }\n";
    assert!(carried_writes("a.rs", read, &needles).is_empty());
    let nested = "fn r(v: Option<Verdict>) -> bool { matches!(v, Some(Verdict::Pass)) }\n";
    assert!(carried_writes("a.rs", nested, &needles).is_empty());
    let write =
        "fn w(b: bool) -> Verdict { if matches!(b, true) { Verdict::Pass } else { x() } }\n";
    assert_eq!(carried_writes("a.rs", write, &needles).len(), 1);
    assert!(!inside_matches("if matches!(b, true) { "));
    assert!(inside_matches("matches!(v, Some("));
}

/// The paths in `v` that end in an empty array: a field set the probes could not see.
fn empty_arrays(path: &str, v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Array(a) if a.is_empty() => out.push(path.to_string()),
        Value::Array(a) => a
            .iter()
            .for_each(|x| empty_arrays(&format!("{path}[]"), x, out)),
        Value::Object(m) => m
            .iter()
            .for_each(|(k, x)| empty_arrays(&format!("{path}/{k}"), x, out)),
        _ => {}
    }
}

/// The replies of the seeded project that still hold an empty array, so the fields of its
/// elements are not pinned, so a field of such an element is outside the contract this fixture
/// pins. They are empty because no project of this size holds the kind (signatures, quarantined
/// frames, upgrade steps, lineage, stale states, code files) or because the one call that fills
/// them is not worth its set-up (a conflict, a moved block, a plan note). The list is explicit
/// so that coverage is not claimed for them: a new empty array fails the test, and a path that
/// fills up must leave the list. ck8: a case of a check that reads no linked checkout holds no
/// code, and an agent check is in no stage (`rdy_schema` lists its stages as an empty array).
/// ck9: the command line's run and request hold no code for the same reason, and the held
/// version's card fills `cli/pending/stdout/not_ready`, which leaves the list. nb8: a neighbor
/// entry fills `mcp/rdy_propose/ok/consequences`, which leaves the list. TASK-1067: every
/// neighbor row carries `values` (`[]` for a created or deleted cause), so the neighbor rows'
/// member is not optional, and the probes' neighbor causes state no literal, so it is empty;
/// `rdy_write`'s rows also hold other kinds without it (D-CFC6V), so its `values?` stays.
/// ab1 (FW-AB1): `rdy_schema` lists the content fields `planned` (bool) and `since` (str), which
/// have no enum values, so their `values` are empty.
/// ain4/ain5: the unknown load refusal states no issue rows. agi6: the instruction probes run
/// with a home of their own and a project with no instruction set, so every row and menu entry
/// is built in, and those store no reason and name no selector; setup in a clean home warns of
/// nothing.
const EMPTY_IN_FIXTURE: &[&str] = &[
    "cli/approve/stdout/consequences[]/because/values",
    "cli/approve/stdout/signatures",
    "cli/approve/stdout/warnings",
    "cli/brief/stdout/inflight/intents",
    "cli/brief/stdout/inflight/proposals",
    "cli/brief/stdout/inflight/tensions",
    "cli/brief/stdout/inflight/views",
    "cli/brief/stdout/lineage",
    "cli/brief/stdout/suspects",
    "cli/brief/stdout/will_go_stale[]/states",
    "cli/compact/stdout/notes",
    "cli/coverage/stdout/kinds",
    "cli/coverage/stdout/similar_kinds",
    "cli/diff/stdout/entries[]/fields",
    "cli/diff/stdout/entries[]/links",
    "cli/diff/stdout/entries[]/repinned",
    "cli/fsck/stdout/open_report/quarantined",
    "cli/fsck/stdout/quarantine",
    "cli/inbox/stdout/entries",
    "cli/instructions full/stdout/instructions[]/reason",
    "cli/instructions full/stdout/menu[]/hints",
    "cli/instructions/stdout/instructions[]/reason",
    "cli/instructions/stdout/menu[]/hints",
    "cli/log/stdout[]/dispute/disputes",
    "cli/log/stdout[]/rebased_from",
    "cli/new doc/stdout/consequences",
    "cli/new doc/stdout/on_change/checks",
    "cli/new doc/stdout/on_change/resolved",
    "cli/new doc/stdout/on_change/tensions",
    "cli/new doc/stdout/suggestions",
    "cli/new doc/stdout/warnings",
    "cli/new folder/stdout/consequences",
    "cli/new folder/stdout/coverage/pages",
    "cli/new folder/stdout/on_change/checks",
    "cli/new folder/stdout/on_change/resolved",
    "cli/new folder/stdout/on_change/tensions",
    "cli/new folder/stdout/suggestions",
    "cli/new folder/stdout/warnings",
    "cli/plan/stdout/notes",
    "cli/propose/stdout/blocking",
    "cli/propose/stdout/conflicts",
    "cli/propose/stdout/consequences",
    "cli/propose/stdout/issues",
    "cli/propose/stdout/plan/items",
    "cli/propose/stdout/tensions",
    "cli/propose/stdout/warnings",
    "cli/publication check against/stdout/disclosure/added_attachments",
    "cli/publication check against/stdout/disclosure/added_docs",
    "cli/publication check against/stdout/disclosure/changed_attachments",
    "cli/publication check against/stdout/disclosure/changed_docs",
    "cli/publication check against/stdout/disclosure/changed_folders",
    "cli/publication check against/stdout/disclosure/changed_outside_docs",
    "cli/publication check against/stdout/disclosure/new_references",
    "cli/publication check against/stdout/disclosure/redactions",
    "cli/publication check against/stdout/disclosure/removed_attachments",
    "cli/publication check against/stdout/references/links_outside/rows",
    "cli/publication check against/stdout/references/rows",
    "cli/publication check against/stdout/warnings",
    "cli/publication check first/stdout/references/links_outside/rows",
    "cli/publication check first/stdout/references/rows",
    "cli/publication check first/stdout/warnings",
    "cli/remote list/stdout/remotes",
    "cli/remote list/stdout/warnings",
    "cli/review request/stdout/case/code",
    "cli/review request/stdout/requests[]/case/code",
    "cli/review run/stdout/case/code",
    "cli/review run/stdout/requests[]/case/code",
    "cli/rule add card/stdout/cards",
    "cli/rule add person/stdout/landing/consequences",
    "cli/rule add person/stdout/landing/signatures",
    "cli/rule add person/stdout/landing/warnings",
    "cli/rule add/stdout/landing?/consequences",
    "cli/rule add/stdout/landing?/signatures",
    "cli/rule add/stdout/landing?/warnings",
    "cli/rule answer/stdout/landing?/consequences",
    "cli/rule answer/stdout/landing?/signatures",
    "cli/rule answer/stdout/landing?/warnings",
    "cli/rule answer/stdout/remaining_cards?[]/card/members",
    "cli/rule edit card/stdout/card/members",
    "cli/rule edit card/stdout/cards[]/card/members",
    "cli/rule edit/stdout/landing/consequences",
    "cli/rule edit/stdout/landing/signatures",
    "cli/rule edit/stdout/landing/warnings",
    "cli/rule remove/stdout/landing/consequences",
    "cli/rule remove/stdout/landing/signatures",
    "cli/rule remove/stdout/landing/warnings",
    "cli/setup/stdout/warnings",
    "cli/show landing/stdout/diff/entries[]/fields",
    "cli/show landing/stdout/diff/entries[]/links",
    "cli/show landing/stdout/diff/entries[]/repinned",
    "cli/show landing/stdout/landing/dispute/disputes",
    "cli/show landing/stdout/landing/rebased_from",
    "cli/show/stdout/suspects",
    "cli/status/stdout/coverage/kinds",
    "cli/status/stdout/coverage/similar_kinds",
    "cli/status/stdout/open_report/quarantined",
    "cli/status/stdout/quarantine",
    "cli/status/stdout/warnings",
    "cli/tag/stdout/signatures",
    "cli/trace/stdout/decisions[]/accepted_in/dispute/disputes",
    "cli/trace/stdout/edges",
    "cli/trace/stdout/frontier",
    "cli/trace/stdout/introduced/dispute/disputes",
    "cli/trace/stdout/premises",
    "cli/trace/stdout/questions",
    "cli/trace/stdout/sources",
    "cli/undo/stdout/cascaded",
    "cli/undo/stdout/conflicts",
    "cli/undo/stdout/consequences",
    "cli/undo/stdout/included",
    "cli/undo/stdout/on_change/checks",
    "cli/undo/stdout/on_change/resolved",
    "cli/undo/stdout/on_change/tensions",
    "cli/upgrade/stdout/steps",
    "cli/view show/stdout/conflicts",
    "mcp/rdy_alternatives list/ok/sets",
    "mcp/rdy_answer elicit mixed/refused/issues",
    "mcp/rdy_answer elicit unavailable/refused/issues",
    "mcp/rdy_answer/ok/landing/|null/consequences[]/because/values",
    "mcp/rdy_answer/ok/landing/|null/warnings",
    "mcp/rdy_answer/ok/signatures?",
    "mcp/rdy_answer/refused/issues",
    "mcp/rdy_brief/ok/inflight/intents",
    "mcp/rdy_brief/ok/inflight/proposals",
    "mcp/rdy_brief/ok/inflight/tensions",
    "mcp/rdy_brief/ok/inflight/views",
    "mcp/rdy_brief/ok/lineage",
    "mcp/rdy_brief/ok/suspects",
    "mcp/rdy_brief/ok/will_go_stale[]/states",
    "mcp/rdy_compact/ok/notes",
    "mcp/rdy_context/ok/children",
    "mcp/rdy_context/ok/suspects",
    "mcp/rdy_context/refused/issues",
    "mcp/rdy_coverage/ok/kinds",
    "mcp/rdy_coverage/ok/similar_kinds",
    "mcp/rdy_decide/ok/consequences",
    "mcp/rdy_decide/ok/on_change/checks",
    "mcp/rdy_decide/ok/on_change/resolved",
    "mcp/rdy_decide/ok/on_change/tensions",
    "mcp/rdy_decide/ok/related_decisions",
    "mcp/rdy_decide/ok/suggestions",
    "mcp/rdy_decide/ok/warnings",
    "mcp/rdy_diff/ok/entries[]/fields",
    "mcp/rdy_diff/ok/entries[]/links",
    "mcp/rdy_diff/ok/entries[]/repinned",
    "mcp/rdy_import reader/refused/issues",
    "mcp/rdy_import unknown key/refused/issues",
    "mcp/rdy_import/ok/consequences",
    "mcp/rdy_import/ok/on_change/checks",
    "mcp/rdy_import/ok/on_change/resolved",
    "mcp/rdy_import/ok/on_change/tensions",
    "mcp/rdy_import/ok/related_decisions",
    "mcp/rdy_import/ok/suggestions",
    "mcp/rdy_instructions full/ok/instructions[]/reason",
    "mcp/rdy_instructions full/ok/menu[]/hints",
    "mcp/rdy_instructions unknown load/refused/issues",
    "mcp/rdy_instructions/ok/instructions[]/reason",
    "mcp/rdy_instructions/ok/menu[]/hints",
    "mcp/rdy_log landing/ok/diff/entries[]/fields",
    "mcp/rdy_log landing/ok/diff/entries[]/links",
    "mcp/rdy_log landing/ok/diff/entries[]/repinned",
    "mcp/rdy_log landing/ok/landing/dispute/disputes",
    "mcp/rdy_log landing/ok/landing/rebased_from",
    "mcp/rdy_log/ok[]/dispute/disputes",
    "mcp/rdy_log/ok[]/rebased_from",
    "mcp/rdy_overview/ok/coverage/kinds",
    "mcp/rdy_overview/ok/coverage/similar_kinds",
    "mcp/rdy_overview/ok/open_report/quarantined",
    "mcp/rdy_pending/ok/not_ready",
    "mcp/rdy_plan/ok/notes",
    "mcp/rdy_propose/ok/blocking",
    "mcp/rdy_propose/ok/conflicts",
    "mcp/rdy_propose/ok/consequences[]/because/values",
    "mcp/rdy_propose/ok/issues",
    "mcp/rdy_publication_check against/ok/disclosure/added_attachments",
    "mcp/rdy_publication_check against/ok/disclosure/added_docs",
    "mcp/rdy_publication_check against/ok/disclosure/changed_attachments",
    "mcp/rdy_publication_check against/ok/disclosure/changed_docs",
    "mcp/rdy_publication_check against/ok/disclosure/changed_folders",
    "mcp/rdy_publication_check against/ok/disclosure/changed_outside_docs",
    "mcp/rdy_publication_check against/ok/disclosure/new_references",
    "mcp/rdy_publication_check against/ok/disclosure/redactions",
    "mcp/rdy_publication_check against/ok/disclosure/removed_attachments",
    "mcp/rdy_publication_check against/ok/references/links_outside/rows",
    "mcp/rdy_publication_check against/ok/references/rows",
    "mcp/rdy_publication_check against/ok/warnings",
    "mcp/rdy_publication_check first/ok/references/links_outside/rows",
    "mcp/rdy_publication_check first/ok/references/rows",
    "mcp/rdy_publication_check first/ok/warnings",
    "mcp/rdy_review request/ok/case/code",
    "mcp/rdy_review request/ok/requests[]/case/code",
    "mcp/rdy_review run/ok/case/code",
    "mcp/rdy_review run/ok/requests[]/case/code",
    "mcp/rdy_review_next decision_review/ok/case/code",
    "mcp/rdy_review_next/ok/case/entities",
    "mcp/rdy_review_next/refused/issues",
    "mcp/rdy_review_submit/refused/issues",
    "mcp/rdy_revoke/ok/consequences",
    "mcp/rdy_revoke/ok/on_change/checks",
    "mcp/rdy_revoke/ok/on_change/resolved",
    "mcp/rdy_revoke/ok/on_change/tensions",
    "mcp/rdy_revoke/ok/suggestions",
    "mcp/rdy_rule add person/ok/landing/consequences",
    "mcp/rdy_rule add person/ok/landing/signatures",
    "mcp/rdy_rule add person/ok/landing/warnings",
    "mcp/rdy_rule add/ok/landing?/consequences",
    "mcp/rdy_rule add/ok/landing?/signatures",
    "mcp/rdy_rule add/ok/landing?/warnings",
    "mcp/rdy_rule add/refused/issues",
    "mcp/rdy_rule answer/ok/landing?/consequences",
    "mcp/rdy_rule answer/ok/landing?/signatures",
    "mcp/rdy_rule answer/ok/landing?/warnings",
    "mcp/rdy_rule answer/ok/remaining_cards?[]/card/members",
    "mcp/rdy_rule answer/refused/issues",
    "mcp/rdy_rule card/ok/cards[]/card/members",
    "mcp/rdy_rule edit/ok/landing/consequences",
    "mcp/rdy_rule edit/ok/landing/signatures",
    "mcp/rdy_rule edit/ok/landing/warnings",
    "mcp/rdy_rule remove/ok/landing/consequences",
    "mcp/rdy_rule remove/ok/landing/signatures",
    "mcp/rdy_rule remove/ok/landing/warnings",
    "mcp/rdy_rule remove/refused/issues",
    "mcp/rdy_schema/ok/checks?/agent[]/stages",
    "mcp/rdy_schema/ok/children?",
    "mcp/rdy_schema/ok/fields?/planned/values",
    "mcp/rdy_schema/ok/fields?/since/values",
    "mcp/rdy_supersede/ok/consequences",
    "mcp/rdy_supersede/ok/on_change/checks",
    "mcp/rdy_supersede/ok/on_change/resolved",
    "mcp/rdy_supersede/ok/on_change/tensions",
    "mcp/rdy_supersede/ok/suggestions",
    "mcp/rdy_supersede/ok/warnings",
    "mcp/rdy_tag/ok/signatures",
    "mcp/rdy_trace/ok/decisions[]/accepted_in?/dispute/disputes",
    "mcp/rdy_trace/ok/frontier",
    "mcp/rdy_trace/ok/introduced/dispute/disputes",
    "mcp/rdy_trace/ok/premises",
    "mcp/rdy_trace/ok/questions",
    "mcp/rdy_trace/ok/sources",
    "mcp/rdy_upgrade/ok/steps",
    "mcp/rdy_view compare/ok/added[]/fields",
    "mcp/rdy_view compare/ok/conflicted",
    "mcp/rdy_view compare/ok/deleted",
    "mcp/rdy_view compare/ok/moved",
    "mcp/rdy_view show/ok/conflicts",
    "mcp/rdy_view undo/refused/issues",
    "mcp/rdy_write/ok/consequences[]/because/values?",
    "mcp/rdy_write/ok/on_change/resolved",
    "mcp/rdy_write/ok/suggestions",
];

#[test]
fn ok14_probes_see_populated_replies() {
    let fixture = read_fixture();
    let mut empty = vec![];
    empty_arrays("cli", &fixture["cli"]["runtime"]["probes"], &mut empty);
    empty_arrays("mcp", &fixture["mcp"]["result_fields"], &mut empty);
    empty.sort();
    empty.dedup();
    let allowed: Vec<String> = EMPTY_IN_FIXTURE.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(
        empty, allowed,
        "empty arrays in the probe shapes hide the fields of their elements"
    );
    // the tension, suspect, pending item and tag the brief names are in the shapes
    let mcp = &fixture["mcp"]["result_fields"];
    assert!(mcp["rdy_tensions"]["ok"]["tensions"][0]["sides"][0]["actor"].is_string());
    assert!(mcp["rdy_suspects"]["ok"]["items"][0]["target"].is_string());
    assert!(mcp["rdy_pending"]["ok"]["items"][0]["card_hash"].is_string());
    assert!(mcp["rdy_tags"]["ok"]["versions"][0]["name"].is_string());
    let cli = &fixture["cli"]["runtime"]["probes"];
    assert!(cli["tensions"]["stdout"]["tensions"][0]["id"].is_string());
    assert!(cli["suspects"]["stdout"]["items"][0]["target"].is_string());
    assert!(cli["pending"]["stdout"]["items"][0]["card_hash"].is_string());
    assert!(cli["tags"]["stdout"]["versions"][0]["name"].is_string());
    // agi6: the instruction replies hold rows and a menu, with their fields in the shape
    for reply in [
        &mcp["rdy_instructions"]["ok"],
        &mcp["rdy_instructions full"]["ok"],
        &cli["instructions"]["stdout"],
        &cli["instructions full"]["stdout"],
    ] {
        assert_eq!(
            shape_keys(&reply["instructions"][0]),
            ["id", "layer", "locked", "reason", "ruleset", "text"],
            "{reply}"
        );
        assert_eq!(
            shape_keys(&reply["menu"][0]),
            ["hints", "id", "layer", "name", "required", "tokens", "when"],
            "{reply}"
        );
    }
    // the writer, answer and review tools were called and answered
    for tool in [
        "rdy_write",
        "rdy_decide",
        "rdy_supersede",
        "rdy_revoke",
        "rdy_propose",
        "rdy_answer",
        "rdy_tag",
        "rdy_resolve",
        "rdy_review_next",
        "rdy_review_submit",
        "rdy_view create",
        "rdy_intent post",
    ] {
        assert!(mcp[tool]["ok"].is_object(), "{tool} has no recorded result");
    }
    for refusal in ["rdy_answer", "rdy_review_submit"] {
        assert!(
            mcp[refusal]["refusal_codes"]
                .as_array()
                .is_some_and(|c| !c.is_empty()),
            "{refusal} has no recorded refusal"
        );
    }
    for cmd in [
        "answer",
        "approve",
        "tag",
        "propose",
        "new doc",
        "new folder",
        "view new",
        "intent post",
    ] {
        assert!(
            cli[cmd]["stdout"].is_object(),
            "{cmd} has no recorded result"
        );
    }
}

#[test]
fn ok14_probes_account_for_every_command() {
    let fixture = read_fixture();
    let probed: Vec<&String> = fixture["cli"]["runtime"]["probes"]
        .as_object()
        .expect("probes")
        .keys()
        .collect();
    let listed = fixture["cli"]["runtime"]["not_probed_commands"]
        .as_object()
        .expect("the commands that are not probed");
    let covered = |path: &str| {
        probed
            .iter()
            .any(|l| *l == path || l.starts_with(&format!("{path} ")))
            || listed.contains_key(path)
    };
    let mut paths = vec![];
    for top in fixture["cli"]["tree"]["subcommands"]
        .as_array()
        .expect("commands")
    {
        let name = top["name"].as_str().expect("name");
        paths.push(name.to_string());
        for sub in top["subcommands"].as_array().into_iter().flatten() {
            paths.push(format!("{name} {}", sub["name"].as_str().expect("name")));
        }
    }
    for path in &paths {
        let parent = path.split(' ').next().unwrap_or_default();
        let probed_here = probed
            .iter()
            .any(|l| *l == path || l.starts_with(&format!("{path} ")));
        let ok = if path.contains(' ') {
            covered(path) || listed.contains_key(parent)
        } else {
            // a command with subcommands is covered when each of them is, or when it is listed
            let has_subs = paths.iter().any(|p| p.starts_with(&format!("{path} ")));
            covered(path) || (has_subs && probed_here)
        };
        assert!(
            ok,
            "the command `{path}` is neither probed nor listed in CLI_NOT_CAPTURED"
        );
    }
    for path in listed.keys() {
        assert!(
            paths.contains(path),
            "`{path}` is listed but is not a command"
        );
        assert!(
            !probed.contains(&path),
            "`{path}` is probed and listed as not probed"
        );
    }
    // every tool is probed or listed, and each unprobed operation is one the tool takes
    let mcp = &fixture["mcp"];
    let results = mcp["result_fields"].as_object().expect("results");
    let skipped = mcp["result_fields_not_captured"]
        .as_object()
        .expect("not captured");
    for name in mcp["tool_names"].as_array().expect("tools") {
        let name = name.as_str().expect("name");
        assert!(
            results
                .keys()
                .any(|k| k == name || k.starts_with(&format!("{name} ")))
                || skipped.contains_key(name),
            "{name} is neither probed nor listed"
        );
    }
    for op in mcp["ops_not_probed"].as_object().expect("ops").keys() {
        let (tool, verb) = op.split_once(' ').expect("`tool op`");
        let text = mcp["tools"][tool]["input_schema"]["properties"]["op"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(
            text.contains(verb),
            "{tool} has no operation {verb}: {text}"
        );
        assert!(!results.contains_key(op), "{op} is probed and listed");
    }
}

#[test]
fn ok14_probe_shapes_merge_every_array_element() {
    let v = json!({"items": [{"a": 1, "keep": {"x": 1}}, {"a": 2, "b": "text"}, {"a": null, "keep": {"y": true}}], "empty": []});
    assert_eq!(
        shape(&v),
        json!({
            "items": [{"a": "null|number", "b?": "string", "keep?": {"x?": "number", "y?": "bool"}}],
            "empty": [],
        })
    );
    // a member named by an id is one entry whatever the id
    assert_eq!(
        shape(&json!({"d-AAAAA": 1, "d-BBBBB": 2, "name": 3})),
        json!({"<id>": "number", "name": "number"})
    );
    // an empty array adds nothing to a merge, and a value after a null keeps its shape
    assert_eq!(
        shape(&json!([[], [{"k": 1}], null])),
        json!([{"|null": [{"k": "number"}]}])
    );
}

/// mh7 (D-4YRQ4, D-KQ4CA): the compact change adds exactly one tool, one CLI command and the byte
/// keys, and takes nothing away. The tool takes `dry_run` and nothing else; the report has six
/// keys, and a row of `by_kind` four; `rdy status --json` and `rdy fsck` gain `log_bytes`,
/// `removed_bytes` and `by_kind`. The daemon op is pinned by `ctl2_ops_are_named_and_an_unknown_op_names_them`
/// alone, since this fixture has no daemon op member.
#[test]
fn mh7_new_surfaces_preserve_the_release_contract() {
    let fixture = read_fixture();
    let mcp = &fixture["mcp"];
    // 57 tools before; one more (and ain4's rdy_instructions and rtc5's rdy_rule), and the earlier ones stay
    let names: Vec<&str> = mcp["tool_names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap())
        .collect();
    // rtc5 adds `rdy_rule`
    assert_eq!(names.len(), 60);
    assert!(names.contains(&"rdy_rule"));
    assert!(names.contains(&"rdy_compact"));
    for kept in [
        "rdy_upgrade",
        "rdy_dump",
        "rdy_load",
        "rdy_clone",
        "rdy_stream_check",
    ] {
        assert!(names.contains(&kept), "{kept}");
    }
    let schema = &mcp["tools"]["rdy_compact"]["input_schema"];
    assert_eq!(schema["additionalProperties"], json!(false));
    assert_eq!(
        shape_keys(&schema["properties"]),
        ["dry_run"],
        "one property"
    );
    assert_eq!(
        schema["properties"]["dry_run"]["description"],
        "true: report bytes without punching holes. Default false."
    );
    // the report: six keys, and four in a row (sorted here; the report serializes them in the
    // order the decision lists: log, removed, punched, on-disk, by kind, notes)
    let report = &mcp["result_fields"]["rdy_compact"]["ok"];
    assert_eq!(
        shape_keys(report),
        [
            "by_kind",
            "log_bytes",
            "notes",
            "on_disk_bytes",
            "punched_bytes",
            "removed_bytes"
        ]
    );
    assert_eq!(
        shape_keys(&report["by_kind"][0]),
        ["bytes", "kind", "name", "removed_bytes"]
    );
    // the command: one flag of its own, and the earlier ones stay
    let tree = &fixture["cli"]["tree"];
    let top: Vec<&str> = tree["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        top.len(),
        COMMANDS_BEFORE_CK9.len() + 5,
        "review, compact, ain5's instructions and setup, and rtc6's rule"
    );
    for kept in COMMANDS_BEFORE_CK9 {
        assert!(top.contains(&kept), "{kept}");
    }
    let compact = tree["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "compact")
        .expect("the compact command");
    assert_eq!(own_args(compact), ["dry_run"]);
    assert_eq!(compact["subcommands"], json!([]));
    // status and fsck gain the byte keys and keep the keys they had
    let runtime = &fixture["cli"]["runtime"]["probes"];
    for (label, kept) in [
        ("status", ["landings", "open_report", "quarantine", "seq"]),
        ("fsck", ["frames", "landings", "open_report", "quarantine"]),
    ] {
        let stdout = &runtime[label]["stdout"];
        let keys = shape_keys(stdout);
        for key in ["log_bytes", "removed_bytes", "by_kind"]
            .iter()
            .chain(&kept)
        {
            assert!(
                keys.iter().any(|k| k == key),
                "{label} lacks {key}: {keys:?}"
            );
        }
        assert_eq!(
            shape_keys(&stdout["by_kind"][0]),
            ["bytes", "kind", "name", "removed_bytes"],
            "{label}"
        );
    }
    // the compact command's own probe
    assert_eq!(
        shape_keys(&runtime["compact"]["stdout"]),
        shape_keys(report)
    );
}

/// The default person place of an isolated home, with a store that must never be read.
struct NoStoreHost(PathBuf);

impl PersonHost for NoStoreHost {
    fn locate(&self) -> Result<PersonSource, RdyError> {
        Ok(PersonSource {
            path: PersonPath::Default(self.0.clone()),
            origin: None,
            warnings: vec![],
            config_file: None,
        })
    }

    fn fetch(
        &self,
        _meta: &Path,
        _load: &LoadRequest,
        _caller: &Caller,
    ) -> Result<Contribution, RdyError> {
        panic!("a missing rules project is not read");
    }
}

/// The rows and menu that the committed guide bundle and tool table say a full reply holds. The
/// final row of a ruleset that owns tools carries its value's line (D-Y18E1); the value hashes the
/// bundle's source rows. Computed here and not by the code under test.
fn bundle_reply(bundle: &Value, table: &Value) -> (Value, Value) {
    let (mut rows, mut menu) = (vec![], vec![]);
    for set in bundle["rulesets"].as_array().expect("rulesets") {
        let handle = format!("rdy:{}", set["name"].as_str().expect("a name"));
        let tools: Vec<&str> = table
            .as_array()
            .expect("the table's rows")
            .iter()
            .filter(|row| row["ruleset"] == handle.as_str())
            .map(|row| row["tool"].as_str().expect("a tool"))
            .collect();
        let source_rows = set["rows"].as_array().expect("rows");
        let source: String = source_rows
            .iter()
            .map(|row| row["text"].as_str().expect("a text"))
            .collect();
        let digest: String = Sha256::digest(source.as_bytes())
            .iter()
            .take(4)
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut bytes = 0usize;
        for (index, row) in source_rows.iter().enumerate() {
            let mut text = row["text"].as_str().expect("a text").to_string();
            if !tools.is_empty() && index + 1 == source_rows.len() {
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&format!(
                    "On every call to a tool in this ruleset, pass kungfu: \"kf-{digest}\". Tools: {}.\n",
                    tools.join(", ")
                ));
            }
            bytes += text.len();
            rows.push(json!({
                "id": row["id"], "ruleset": handle, "text": text, "layer": "rdy",
                "locked": row["locked"], "reason": [],
            }));
        }
        menu.push(json!({
            "id": handle, "name": set["title"], "when": set["when"], "layer": "rdy",
            "required": set["required"], "tokens": bytes.div_ceil(4), "hints": [],
        }));
    }
    (Value::Array(rows), Value::Array(menu))
}

/// agi6 (D-GEKKY): in an isolated home the CLI, MCP and the engine deliver the same built-in rows
/// and menu, and those are the committed bundle's; the release's own checker accepts this binary.
#[test]
fn agi6_release_surface_exposes_full_and_builtin_rows() {
    let world = rdy_qa::instructions::World::new();
    let bundle_path = workspace_root().join("release/agent-guide.json");
    let bundle: Value =
        serde_json::from_slice(&std::fs::read(&bundle_path).expect("the bundle")).expect("JSON");
    let table_path = workspace_root().join("crates/rdy-engine/kungfu-tools.json");
    let table: Value =
        serde_json::from_slice(&std::fs::read(&table_path).expect("the table")).expect("JSON");
    let (rows, menu) = bundle_reply(&bundle, &table);
    assert_eq!(rows.as_array().unwrap().len(), 14);
    let lines = rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["text"].as_str().unwrap().contains("pass kungfu: \"kf-"))
        .count();
    assert_eq!(lines, 9, "one line for each ruleset that owns tools");
    assert_eq!(menu.as_array().unwrap().len(), 12);
    let required: Vec<&Value> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["ruleset"] == "rdy:bootstrap")
        .collect();
    assert_eq!(required.len(), 3);

    // the engine, with no project
    let load = LoadRequest::parse(&[]).expect("no load");
    let caller = Caller {
        actor: "agent:qa".into(),
        run: None,
    };
    let host = NoStoreHost(world.default_meta());
    let engine = |load: &LoadRequest| {
        serde_json::to_value(deliver(None, load, &caller, Some(&host)).expect("deliver"))
            .expect("a reply")
    };
    let full = engine(&load.clone().with_full(true));
    assert_eq!(full["instructions"], rows);
    assert_eq!(full["menu"], menu);
    let base = engine(&load);
    assert_eq!(
        base["instructions"],
        Value::Array(required.iter().map(|r| (*r).clone()).collect()),
        "the bootstrap alone arrives without full"
    );
    assert_eq!(base["menu"], menu, "the menu is the same either way");

    // the CLI and both shapes of MCP, in a project and with no project
    let dir = world.project();
    let (ok, cli) = world.json(dir.path(), &["instructions", "--full"]);
    assert!(ok, "{cli}");
    assert_eq!(cli["instructions"], rows);
    assert_eq!(cli["menu"], menu);
    let (ok, cli_base) = world.json(dir.path(), &["instructions"]);
    assert!(ok, "{cli_base}");
    assert_eq!(cli_base["instructions"], base["instructions"]);
    assert_eq!(cli_base["menu"], menu);
    let mut c = world.mcp(dir.path());
    assert!(
        c.call(
            "rdy_session",
            json!({"actor": "agent:qa", "run": "agi6", "role": "reader"})
        )
        .is_ok()
    );
    let mcp = c.call("rdy_instructions", json!({"full": true})).ok();
    assert_eq!(mcp, cli, "the same JSON from MCP and the CLI");
    let mcp_base = c.call("rdy_instructions", json!({})).ok();
    assert_eq!(mcp_base, cli_base);
    drop(c);
    let elsewhere = tempfile::tempdir().expect("a folder with no project");
    let mut outside = rdy_qa::fixtures::rdy_command();
    outside
        .args(["--json", "instructions", "--full"])
        .envs(world.env())
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .current_dir(elsewhere.path());
    let out = output_within(&mut outside, Duration::from_secs(120)).expect("run rdy");
    assert!(out.status.success(), "{out:?}");
    let reply: Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(reply["instructions"], rows);
    assert_eq!(reply["menu"], menu);

    // the input and the flag are in the public surface
    let fixture = read_fixture();
    let schema = &fixture["mcp"]["tools"]["rdy_instructions"]["input_schema"]["properties"];
    assert_eq!(shape_keys(schema), ["full", "load"]);
    let command = fixture["cli"]["tree"]["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "instructions")
        .expect("the instructions command");
    assert_eq!(own_args(command), ["load", "full"]);

    // the check that the release runs on the packaged binary accepts this binary as it stands
    let python = std::process::Command::new("python3")
        .arg("--version")
        .output();
    if !python.is_ok_and(|o| o.status.success()) {
        skip::skip(
            &rdy_qa::test_name!("agi6_release_surface_exposes_full_and_builtin_rows"),
            Why::Prerequisite,
            "python3 is not on PATH",
        );
        return;
    }
    let checked = std::process::Command::new("python3")
        .args(["-I", "-B"])
        .arg(workspace_root().join("scripts/release/guide.py"))
        .arg("check-binary")
        .arg("--binary")
        .arg(rdy_qa::fixtures::rdy_bin())
        .arg("--bundle")
        .arg(&bundle_path)
        .arg("--sidecar")
        .arg(workspace_root().join("release/agent-guide.sha256"))
        .arg("--table")
        .arg(&table_path)
        .output()
        .expect("run the checker");
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
}

/// kfu6 (D-Y18E1): the release surface keeps the argument undisclosed. Every tool's input schema
/// stays closed and holds no word of it; the errors reference names only the code `W_KUNGFU`,
/// with no parameter, no way to send a value and no value.
#[test]
fn kfu6_release_surface_keeps_argument_undisclosed() {
    let fixture = read_fixture();
    let tools = fixture["mcp"]["tools"].as_object().expect("the tools");
    assert!(tools.len() > 20, "the tools are in the fixture");
    for (name, tool) in tools {
        let schema = &tool["input_schema"];
        assert_eq!(
            schema["additionalProperties"],
            json!(false),
            "{name} is closed"
        );
        let text = schema.to_string().to_lowercase();
        assert!(
            !text.contains("kungfu") && !text.contains("kf-"),
            "{name}: {text}"
        );
        assert!(
            schema["properties"].get("kungfu").is_none(),
            "{name} lists the argument"
        );
    }
    // the live listing is the fixture's
    let (d, r) = rdy_qa::fixtures::project();
    drop(r);
    let mut c = isolated_mcp(d.path());
    for t in c.tools() {
        let name = t["name"].as_str().expect("a name");
        let text = t["inputSchema"].to_string().to_lowercase();
        assert!(!text.contains("kungfu") && !text.contains("kf-"), "{name}");
        assert_eq!(
            t["inputSchema"]["additionalProperties"],
            json!(false),
            "{name}"
        );
    }
    drop(c);

    // the code row: the code name, the reference text, and nothing of the argument
    let rows = fixture["errors_warnings"]["error_rows"]
        .as_array()
        .expect("error rows");
    let row = rows
        .iter()
        .find(|r| r[0] == "W_KUNGFU")
        .expect("a W_KUNGFU row");
    assert_eq!(
        row,
        &json!([
            "W_KUNGFU",
            "A call lacks the current value from its tool's ruleset. The call still runs. The warning asks the agent to reload its instructions.",
            "Call rdy_instructions and load the ruleset named by the warning."
        ])
    );
    let errors = fixture["errors_warnings"].to_string();
    assert_eq!(
        errors.matches("W_KUNGFU").count(),
        errors.to_lowercase().matches("kungfu").count(),
        "the code name is the only mention of the word in the errors surface"
    );
    assert!(!errors.contains("kf-"), "no value in the errors surface");
    assert!(
        fixture["errors_warnings"]["codes"]["W_KUNGFU"].is_string(),
        "the code constant is listed"
    );
    let refusal_rows = fixture["mcp"]["refusal_error_rows"]
        .as_array()
        .expect("rows");
    assert_eq!(
        refusal_rows.iter().filter(|c| *c == "W_KUNGFU").count(),
        1,
        "listed once, before the plan rows"
    );
    let at = refusal_rows.iter().position(|c| c == "W_KUNGFU").unwrap();
    assert_eq!(refusal_rows[at + 1], "E_PLAN_ITEM");
}

/// kfu6 (D-Y18E1): the packaged binary delivers, through the CLI and both MCP servers (in the
/// process and through the daemon proxy), the checked source rows plus exact suffixes. The
/// expectation is computed here from the committed bundle and tool table, byte by byte: the source
/// stays an exact prefix of each final row, the suffix is the one generated line, and every other
/// row is the source unchanged.
#[test]
fn kfu6_packaged_delivery_matches_checked_source_plus_suffix() {
    let world = rdy_qa::instructions::World::new();
    let bundle: Value = serde_json::from_slice(
        &std::fs::read(workspace_root().join("release/agent-guide.json")).expect("the bundle"),
    )
    .expect("JSON");
    let table: Value = serde_json::from_slice(
        &std::fs::read(workspace_root().join("crates/rdy-engine/kungfu-tools.json"))
            .expect("the table"),
    )
    .expect("JSON");
    let (rows, menu) = bundle_reply(&bundle, &table);
    // the suffix rules, apart from the expectation: source prefix, one line, LF ending
    let mut sources = vec![];
    for set in bundle["rulesets"].as_array().expect("rulesets") {
        sources.extend(set["rows"].as_array().expect("rows").iter().cloned());
    }
    assert_eq!(sources.len(), rows.as_array().unwrap().len());
    let mut suffixed = 0;
    for (source, row) in sources.iter().zip(rows.as_array().unwrap()) {
        let (raw, text) = (
            source["text"].as_str().unwrap(),
            row["text"].as_str().unwrap(),
        );
        assert!(text.starts_with(raw) || text.starts_with(&format!("{raw}\n")));
        if text == raw {
            continue;
        }
        suffixed += 1;
        let tail = text.strip_prefix(raw).unwrap();
        let tail = if raw.ends_with('\n') {
            tail
        } else {
            tail.strip_prefix('\n').expect("one LF")
        };
        assert!(
            tail.starts_with("On every call to a tool in this ruleset, pass kungfu: \"kf-")
                && tail.ends_with(".\n")
                && tail.matches('\n').count() == 1
                && !tail.contains('`'),
            "{tail:?}"
        );
    }
    assert_eq!(suffixed, 9, "nine owning rulesets, no other row");

    // CLI
    let dir = world.project();
    let (ok, cli) = world.json(dir.path(), &["instructions", "--full"]);
    assert!(ok, "{cli}");
    assert_eq!(cli["instructions"], rows);
    assert_eq!(cli["menu"], menu);
    // MCP in the process
    let mut c = world.mcp(dir.path());
    assert!(
        c.call(
            "rdy_session",
            json!({"actor": "agent:qa", "run": "kfu6", "role": "reader"})
        )
        .is_ok()
    );
    let direct = c.call("rdy_instructions", json!({"full": true})).ok();
    assert_eq!(direct["instructions"], rows);
    assert_eq!(direct["menu"], menu);
    drop(c);
    // MCP through the daemon proxy
    let served = rdy_qa::daemon::Served::new();
    let mut p = rdy_qa::mcp::McpClient::start_proxy(&served);
    assert!(
        p.call(
            "rdy_session",
            json!({"actor": "agent:qa", "run": "kfu6p", "role": "reader"})
        )
        .is_ok()
    );
    let proxied = p.call("rdy_instructions", json!({"full": true})).ok();
    assert_eq!(proxied["instructions"], rows);
    assert_eq!(proxied["menu"], menu);
}

/// kfu6 (D-Y18E1): the contract holds a deterministic nonzero capture. Status carries the summary
/// and every field of every retained row, in order, with the days named from today; the overview
/// carries the same summary and no rows.
#[test]
fn kfu6_positive_count_rows_are_in_the_contract() {
    let fixture = read_fixture();
    let k = &fixture["mcp"]["kungfu_counts"];
    let line = "kungfu: 4 not loaded, 3 lost, 1 wrong, in 3 sessions.";
    assert_eq!(
        k["template"],
        "kungfu: <not_loaded> not loaded, <lost> lost, <wrong> wrong, in <sessions> sessions."
    );
    assert_eq!(k["zero_omits"], json!(["kungfu", "kungfu_counts"]));
    assert_eq!(k["status"]["kungfu"], line);
    assert_eq!(k["status"]["text_lines"], 1);
    assert_eq!(
        k["status"]["row_members"],
        json!([
            "actor",
            "day",
            "lost",
            "not_loaded",
            "ruleset",
            "run",
            "wrong"
        ])
    );
    assert_eq!(
        k["status"]["kungfu_counts"],
        json!([
            {"day": "today-5", "actor": "agent:a3", "run": "r9", "ruleset": "approvals",
             "not_loaded": 1, "lost": 0, "wrong": 0},
            {"day": "today", "actor": "agent:a1", "run": "r1", "ruleset": "read",
             "not_loaded": 1, "lost": 0, "wrong": 0},
            {"day": "today", "actor": "agent:a1", "run": "r1", "ruleset": "write",
             "not_loaded": 2, "lost": 1, "wrong": 0},
            {"day": "today", "actor": "agent:a2", "run": null, "ruleset": "bootstrap",
             "not_loaded": 0, "lost": 2, "wrong": 1},
        ])
    );
    assert_eq!(
        k["overview"],
        json!({"kungfu": line, "kungfu_counts": null})
    );
    // the capture is live: the generation made now equals it
    assert_eq!(rows()["mcp"]["kungfu_counts"], *k);
}

/// kfu6 (D-Y18E1): the combined diff of the contract is the explained sections alone. The code
/// row (`errors_warnings`: the code constant, its reference row, its schema shape, and `mcp`: its
/// place in the refusal rows) and the positive-count capture (`mcp.kungfu_counts`) are the only
/// movement. The store, dump, sync, signing and export sections keep their bytes: their digests
/// are pinned here from the baseline before this change.
#[test]
fn kfu6_release_contract_keeps_unrelated_bytes() {
    // the released sections' bytes: the frozen baseline, not the current fixture
    let fixture = read_released();
    let digest = |v: &Value| -> String {
        Sha256::digest(serde_json::to_vec(v).expect("encode"))
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let pinned: [(&str, &str); 6] = PINNED_SECTIONS;
    for (name, want) in pinned {
        assert_eq!(digest(&fixture[name]), want, "section {name} moved");
    }
    // the explained members are there
    assert!(fixture["errors_warnings"]["codes"]["W_KUNGFU"].is_string());
    assert!(fixture["mcp"]["kungfu_counts"].is_object());
}

/// The digests of the sections that the kungfu change leaves alone.
/// Release 1 (TASK-1830) moves `store` once: `native_fixture` records the published, frozen
/// `feature-1/expect.json`; the rest of the section keeps its bytes.
const PINNED_SECTIONS: [(&str, &str); 6] = [
    (
        "store",
        "486b25913de3831beb7d8b30d3c738e5c70ff11b639d17fa2b361ecbd688f703",
    ),
    (
        "dump_load",
        "000ea7a593a8e2f4fb0df14e53c463bb9651cc4ceee54d4578c428a1117107e6",
    ),
    (
        "sync",
        "e48c8c397ebd123751f4fea45216dc5c16e1a501efc86b67492d3df3101ec480",
    ),
    (
        "signatures",
        "ae9a90f479e7d64e2eb2156c26d81df4de75524c731cfed245d8e1b82eb07509",
    ),
    (
        "markdown_exports",
        "218ea793688320c47ee1b3c7e032d9c4bbac52c6404926993148f82e11ce4644",
    ),
    (
        "site_urls",
        "2d4c1e7028375201fa9569f76ebeef713997a72dde29d93487b3cb203affd78f",
    ),
];

/// The SHA-256 of `v`'s compact encoding. Objects have sorted keys, so the bytes are the same on
/// every run.
fn digest_of(v: &Value) -> String {
    Sha256::digest(serde_json::to_vec(v).expect("encode"))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// rtc7 (D-ZSRBN): the release contract holds every stop of `rdy_rule`. A guard: Tasks 1, 5 and 6
/// captured the rows, and this pins them together. The input has two closed forms and one tool
/// list; the policy field has two values; the command line has four commands with every flag;
/// and the success, the card, the not-ready stop and the refusals each hold a populated reply, so
/// no stop is pinned by an empty array alone.
#[test]
fn rtc7_release_contract_covers_every_rule_stop() {
    let fixture = read_fixture();
    let mcp = &fixture["mcp"];

    // the tool list and the input: eleven keys, one closed object, `scope` alone required
    let names = mcp["tool_names"].as_array().expect("names");
    assert_eq!(names.len(), 60);
    assert_eq!(names.iter().filter(|n| *n == "rdy_rule").count(), 1);
    let schema = &mcp["tools"]["rdy_rule"]["input_schema"];
    assert_eq!(schema["additionalProperties"], json!(false));
    assert_eq!(schema["required"], json!(["scope"]));
    assert_eq!(
        shape_keys(&schema["properties"]),
        [
            "answer",
            "applies_to",
            "evidence",
            "id",
            "op",
            "question",
            "ruleset",
            "scope",
            "session_ref",
            "text",
            "why"
        ]
    );
    let answer = &schema["$defs"]["RuleAnswerArgs"];
    assert_eq!(answer["additionalProperties"], json!(false));
    assert_eq!(
        answer["required"],
        json!(["card_hash", "evidence", "option", "ref"])
    );
    assert_eq!(
        shape_keys(&answer["properties"]),
        ["card_hash", "evidence", "option", "question", "ref"]
    );
    assert_eq!(answer["properties"]["card_hash"]["minLength"], json!(64));
    assert_eq!(answer["properties"]["card_hash"]["maxLength"], json!(64));

    // success: add, edit and remove carry the rule, the decision and a landing
    let results = &mcp["result_fields"];
    for label in [
        "rdy_rule add",
        "rdy_rule add person",
        "rdy_rule edit",
        "rdy_rule remove",
    ] {
        let ok = &results[label]["ok"];
        assert_eq!(
            shape_keys(&ok["rule"]),
            ["id", "retired", "ruleset", "text"],
            "{label}"
        );
        assert_eq!(shape_keys(&ok["project"]), ["name", "repo_id"], "{label}");
        for key in ["decision", "scope"] {
            assert!(ok[key].is_string(), "{label} lacks {key}");
        }
        let landing = if ok["landing"].is_object() {
            &ok["landing"]
        } else {
            &ok["landing?"]
        };
        assert!(landing["landing"].is_string(), "{label}: {ok}");
        assert_eq!(landing["relayed_by"]["actor"], "string", "{label}");
    }

    // the not-ready stop: a ref and issues that hold a row, with the fields of a row
    let stop = &results["rdy_rule add"]["ok"];
    assert!(stop["ref?"].is_string(), "{stop}");
    assert!(stop["view?"].is_string(), "{stop}");
    assert!(stop["state?"].is_string(), "{stop}");
    assert_eq!(
        shape_keys(&stop["issues?"][0]),
        ["block", "code", "fix", "message", "severity"],
        "a populated issue row"
    );

    // the card stop: the card, its hash, and the attached cards in a row of their own
    let card = &results["rdy_rule card"]["ok"];
    for key in ["card_hash", "proposal_ref", "ref", "state", "view"] {
        assert!(card[key].is_string(), "{key} in {card}");
    }
    assert!(card["card"]["card_hash"].is_string());
    assert!(
        card["card"]["options"][0]["id"].is_string(),
        "a card with options"
    );
    assert_eq!(
        shape_keys(&card["cards"][0]),
        ["card", "card_hash", "ref"],
        "an attached card"
    );
    assert!(card["cards"][0]["card"]["options"][0]["id"].is_string());
    // a rule card carries no card-words warning, so its reply has no warnings member
    assert!(card.get("warnings").is_none(), "{card}");

    // the answer: a landing or the remaining cards, and its answer record
    let answered = &results["rdy_rule answer"]["ok"];
    assert_eq!(
        shape_keys(&answered["answer?"]),
        [
            "act",
            "answer",
            "assurance",
            "label",
            "landing",
            "next",
            "option",
            "ref",
            "relayed_by"
        ]
    );
    assert_eq!(
        shape_keys(&answered["remaining_cards?"][0]),
        ["card", "card_hash", "ref"]
    );
    assert!(answered["landing?"]["landing"].is_string());

    // the refusals: each probed tool names its codes, and the envelope has its four members
    for (label, code) in [
        ("rdy_rule add", "E_BAD_INPUT"),
        ("rdy_rule add", "E_PERMISSION"),
        ("rdy_rule answer", "E_BAD_INPUT"),
        ("rdy_rule remove", "E_NOT_FOUND"),
    ] {
        let codes = results[label]["refusal_codes"].as_array().expect("codes");
        assert!(codes.iter().any(|c| c == code), "{label}: {code}");
        assert_eq!(
            shape_keys(&results[label]["refused"]),
            ["code", "fix", "issues", "message"],
            "{label}"
        );
    }

    // the policy: an optional enum field with the two values, and a probe for each reading
    let policy = &fixture["schema_packs"]["rdy_standard"]["block_types"]["authority_policy"]["fields"]
        ["rules_from_chat"];
    assert_eq!(
        policy,
        &json!({"kind": "enum", "required": false, "values": ["land", "card"]})
    );
    let validation = &fixture["schema_packs"]["validation"];
    for probe in [
        "policy_rules_from_chat_default",
        "policy_rules_from_chat_land",
        "policy_rules_from_chat_card",
        "policy_rules_from_chat_bad_kind",
        "policy_rules_from_chat_bad_value",
    ] {
        assert!(!validation[probe].is_null(), "{probe}");
    }

    // the command line: four commands, in this order, with every flag and positional
    let tree = &fixture["cli"]["tree"]["subcommands"];
    let rule = tree
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "rule")
        .expect("the rule command");
    assert_eq!(rule["subcommand_required"], json!(true));
    let commands: Vec<&str> = rule["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(commands, ["add", "edit", "remove", "answer"]);
    let words = ["why", "evidence", "relayed_by", "question", "session_ref"];
    let content = ["scope", "text", "ruleset", "applies_to"];
    let flags = |name: &str| -> Vec<String> {
        let c = rule["subcommands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap();
        let mut got = own_args(c);
        got.sort();
        got
    };
    let sorted = |mut v: Vec<&str>| -> Vec<String> {
        v.sort();
        v.into_iter().map(String::from).collect()
    };
    assert_eq!(flags("add"), sorted([&content[..], &words[..]].concat()));
    assert_eq!(
        flags("edit"),
        sorted([&["id"][..], &content[..], &words[..]].concat())
    );
    assert_eq!(
        flags("remove"),
        sorted(vec![
            "id",
            "scope",
            "why",
            "evidence",
            "relayed_by",
            "question",
            "session_ref"
        ])
    );
    assert_eq!(
        flags("answer"),
        sorted(vec![
            "ref",
            "option",
            "scope",
            "card",
            "evidence",
            "relayed_by",
            "question"
        ])
    );
    // the commands that need an agent's words require them, and the rest are optional
    for command in rule["subcommands"].as_array().unwrap() {
        for arg in command["args"].as_array().unwrap() {
            let id = arg["id"].as_str().unwrap();
            if matches!(id, "evidence" | "relayed_by" | "scope") {
                assert_eq!(arg["required"], json!(true), "{} {id}", command["name"]);
            }
            if matches!(id, "question" | "session_ref" | "ruleset" | "applies_to") {
                assert_ne!(arg["required"], json!(true), "{} {id}", command["name"]);
            }
        }
    }

    // the command line's replies, run by the release binary: each stop has a populated probe
    let probes = &fixture["cli"]["runtime"]["probes"];
    for label in [
        "rule add",
        "rule add card",
        "rule add person",
        "rule edit",
        "rule edit card",
        "rule remove",
        "rule answer",
    ] {
        assert!(probes[label]["stdout"].is_object(), "{label}");
    }
    assert_eq!(probes["rule answer"]["exits"], json!([0, 1]));
    assert_eq!(probes["rule remove"]["exits"], json!([0, 1]));
    assert!(
        probes["rule answer"]["stderr"]["code"].is_string(),
        "an answer that refuses names its code"
    );
    assert_eq!(
        shape_keys(&probes["rule add"]["stdout"]["issues?"][0]),
        ["block", "code", "fix", "message", "severity"]
    );
    assert!(probes["rule add card"]["stdout"]["card"]["options"][0]["id"].is_string());
    assert!(
        probes["rule edit card"]["stdout"]["cards"][0]["card"]["options"][0]["id"].is_string(),
        "the edit's confirm card"
    );
    assert!(probes["rule answer"]["stdout"]["remaining_cards?"][0]["ref"].is_string());

    // the probes run again give the same shapes
    let live = rows();
    assert_eq!(
        live["mcp"]["result_fields"],
        fixture["mcp"]["result_fields"]
    );
    assert_eq!(
        live["cli"]["runtime"]["probes"],
        fixture["cli"]["runtime"]["probes"]
    );
}

/// rtc7 (D-ZSRBN): a guard. The packaged tools keep the card hash for every ordinary text answer.
/// `rdy_answer` refuses a relayed answer that names no card, with the same words in text and card
/// kinds, and lands nothing; the card's own hash answers it. Only `rdy_rule`'s own cards take a
/// text relay.
#[test]
fn rtc7_release_surface_keeps_other_answer_hashes() {
    use rdy_qa::fixtures::write;
    let (dir, mut r) = project();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "doc", "id": "$d",
                "fields": {"name": "plain", "title": "Plain", "doc_type": "page"}}]),
    )
    .expect("write the doc");
    let p = r
        .propose(
            &rdy_qa::fixtures::writer(),
            "work",
            "Add a doc",
            Some(rdy_qa::intake::TEST_SUMMARY),
        )
        .expect("propose it");
    drop(r);
    let mut c = isolated_mcp(dir.path());
    assert!(
        c.call(
            "rdy_session",
            json!({"actor": "agent:rtc7", "run": "rtc7", "role": "writer"})
        )
        .is_ok()
    );
    let queue = c.call("rdy_pending", json!({})).ok();
    let item = queue["items"]
        .as_array()
        .and_then(|items| items.iter().find(|i| i["ref"] == p.r#ref.as_str()))
        .cloned()
        .unwrap_or_else(|| panic!("the proposal is pending: {queue}"));
    let landings_before = c.call("rdy_log", json!({})).ok();
    for kind in [json!(null), json!("card"), json!("text")] {
        let mut args = json!({"ref": item["ref"], "option": item["options"][0]["id"],
            "evidence": "Yes, that is fine.", "question": "Shall I approve it?"});
        if !kind.is_null() {
            args["kind"] = kind.clone();
        }
        match c.call("rdy_answer", args) {
            rdy_qa::mcp::Reply::ToolError(e) => {
                assert_eq!(e["code"], "E_BAD_INPUT", "{kind}: {e}");
                assert_eq!(
                    e["message"], "a relayed answer names the card the person was shown",
                    "{kind}"
                );
            }
            other => panic!("{kind}: a text answer with no card hash was not refused: {other:?}"),
        }
    }
    // the rule tool's answer form needs the hash by its schema, before any call reaches a store
    match c.call(
        "rdy_rule",
        json!({"scope": "project", "answer": {"ref": item["ref"], "option": "approve",
            "evidence": "Yes."}}),
    ) {
        rdy_qa::mcp::Reply::ToolError(e) => assert_eq!(e["code"], "E_BAD_INPUT", "{e}"),
        rdy_qa::mcp::Reply::RpcError(_) => {}
        other => panic!("an answer with no card hash was not refused: {other:?}"),
    }
    assert_eq!(
        c.call("rdy_log", json!({})).ok(),
        landings_before,
        "nothing landed"
    );
    // an ordinary proposal is no rule proposal: the rule tool will not answer it
    match c.call(
        "rdy_rule",
        json!({"scope": "project", "answer": {"ref": item["ref"], "option": "approve",
            "card_hash": item["card_hash"], "evidence": "Yes."}}),
    ) {
        rdy_qa::mcp::Reply::ToolError(e) => assert_eq!(e["code"], "E_BAD_INPUT", "{e}"),
        other => panic!("another flow's proposal was routed: {other:?}"),
    }
    // the card's own hash answers it
    let done = c.call(
        "rdy_answer",
        json!({"ref": item["ref"], "option": item["options"][0]["id"],
            "card_hash": item["card_hash"], "evidence": "Yes, that is fine.",
            "question": "Shall I approve it?"}),
    );
    assert!(done.is_ok(), "{done:?}");
}

/// rtc7 (D-GEKKY, D-Y18E1): the guide keeps its source apart from the lines that the build
/// generates. The committed bundle is a development source: its edition ends in `-dev`, so no
/// release of this version can name it, and no source row holds a generated line. The delivered
/// write ruleset ends with the source plus one exact suffix, whose tool list holds `rdy_rule`
/// beside the other write tools and no other ruleset's tool.
#[test]
fn rtc7_release_guide_keeps_source_and_generated_suffix_separate() {
    let bundle: Value = serde_json::from_slice(
        &std::fs::read(workspace_root().join("release/agent-guide.json")).expect("the bundle"),
    )
    .expect("JSON");
    let table: Value = serde_json::from_slice(
        &std::fs::read(workspace_root().join("crates/rdy-engine/kungfu-tools.json"))
            .expect("the table"),
    )
    .expect("JSON");
    let sidecar = std::fs::read_to_string(workspace_root().join("release/agent-guide.sha256"))
        .expect("the sidecar");

    // development source: the edition says so and cannot equal the package version, which is
    // the check that production eligibility makes against the release it ships
    // A release commit S holds the public bundle from the frozen landing L instead: its edition
    // is the package version and it names L (checklist step 3).
    let edition = bundle["edition"].as_str().expect("an edition");
    if edition.ends_with("-dev") {
        assert_ne!(edition, env!("CARGO_PKG_VERSION"));
        assert!(
            edition
                .trim_end_matches("-dev")
                .split('.')
                .all(|n| n.parse::<u64>().is_ok()),
            "{edition}"
        );
    } else {
        assert_eq!(edition, env!("CARGO_PKG_VERSION"), "a release bundle");
        let landing = bundle["design_landing"].as_str().expect("a landing");
        assert!(
            landing.len() == 64
                && landing
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{landing}"
        );
    }
    // the source holds no generated line and no digest of its own
    let mut sources = vec![];
    for set in bundle["rulesets"].as_array().expect("rulesets") {
        sources.extend(set["rows"].as_array().expect("rows").iter().cloned());
    }
    for row in &sources {
        let text = row["text"].as_str().expect("a text");
        assert!(
            !text.contains("pass kungfu:") && !text.contains("kf-") && !text.contains("Tools: "),
            "{}",
            row["id"]
        );
    }
    let digest: String =
        Sha256::digest(std::fs::read(workspace_root().join("release/agent-guide.json")).unwrap())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
    assert_eq!(
        sidecar,
        format!("{digest}\n"),
        "the sidecar names these bytes"
    );

    // the write ruleset's tools, from the table, in order
    let write_tools: Vec<&str> = table
        .as_array()
        .expect("rows")
        .iter()
        .filter(|row| row["ruleset"] == "rdy:write")
        .map(|row| row["tool"].as_str().unwrap())
        .collect();
    assert_eq!(
        write_tools,
        [
            "rdy_accept",
            "rdy_asset",
            "rdy_decide",
            "rdy_intent",
            "rdy_revoke",
            "rdy_rule",
            "rdy_supersede",
            "rdy_write"
        ]
    );
    // no other ruleset lists the rule tool
    assert_eq!(
        table
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["tool"] == "rdy_rule")
            .count(),
        1
    );

    // delivered: the packaged binary's write row is the source and the exact suffix
    let world = rdy_qa::instructions::World::new();
    let dir = world.project();
    let (ok, full) = world.json(dir.path(), &["instructions", "--full"]);
    assert!(ok, "{full}");
    let rows = full["instructions"].as_array().expect("rows");
    let delivered = rows
        .iter()
        .find(|r| r["id"] == "rdy:write:body")
        .expect("the write row");
    let source = sources
        .iter()
        .find(|r| r["id"] == "rdy:write:body")
        .expect("the write source");
    let raw = source["text"].as_str().unwrap();
    let text = delivered["text"].as_str().unwrap();
    let kf: String = Sha256::digest(
        bundle["rulesets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "write")
            .unwrap()["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["text"].as_str().unwrap())
            .collect::<String>()
            .as_bytes(),
    )
    .iter()
    .take(4)
    .map(|b| format!("{b:02x}"))
    .collect();
    let suffix = format!(
        "On every call to a tool in this ruleset, pass kungfu: \"kf-{kf}\". Tools: rdy_accept, rdy_asset, rdy_decide, rdy_intent, rdy_revoke, rdy_rule, rdy_supersede, rdy_write.\n"
    );
    let joiner = if raw.ends_with('\n') { "" } else { "\n" };
    assert_eq!(text, format!("{raw}{joiner}{suffix}"));
    // the generated line is in no other row, and the rule tool is named in one suffix alone
    let named = rows
        .iter()
        .filter(|r| r["text"].as_str().unwrap().contains("rdy_rule, "))
        .count();
    assert_eq!(named, 1, "one generated list names rdy_rule");

    // the checker accepts the committed bundle and its sidecar
    let python = std::process::Command::new("python3")
        .arg("--version")
        .output();
    if !python.is_ok_and(|o| o.status.success()) {
        skip::skip(
            &rdy_qa::test_name!("rtc7_release_guide_keeps_source_and_generated_suffix_separate"),
            Why::Prerequisite,
            "python3 is not on PATH",
        );
        return;
    }
    let checked = std::process::Command::new("python3")
        .args(["-I", "-B"])
        .arg(workspace_root().join("scripts/release/guide.py"))
        .arg("check")
        .arg("--bundle")
        .arg(workspace_root().join("release/agent-guide.json"))
        .arg("--sidecar")
        .arg(workspace_root().join("release/agent-guide.sha256"))
        .output()
        .expect("run the checker");
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&checked.stdout).trim(), digest);
}

/// rtc7 (D-ZSRBN): the contract moves for the rules and for nothing else of this plan. A guard:
/// the sections that hold the store's encodings, the dump promise, the stream and the signing
/// contracts keep their bytes, and so does every tool, command and probe except the rule's own.
/// The digests are of the baseline before the rules, taken from the contract at the plan's base
/// (the kungfu change moved only `W_KUNGFU` and `mcp.kungfu_counts`, and those are left out of the
/// error rows and the schema reply).
#[test]
fn rtc7_contract_diff_is_limited_to_rules() {
    // the released sections' bytes: the frozen baseline, not the current fixture
    let fixture = read_released();
    // the frozen sections
    for (name, want) in PINNED_SECTIONS {
        assert_eq!(digest_of(&fixture[name]), want, "section {name} moved");
    }
    assert_eq!(
        digest_of(&fixture["working_copy"]),
        "d9d194b59f3670ba311f7b60e9a28550add6f14b8095e948158d17bea3f09a6a",
        "section working_copy moved"
    );
    // relc5 added `config.listing`; the section's earlier digest is of the rest
    let mut config = fixture["config"].clone();
    config
        .as_object_mut()
        .expect("config")
        .remove("listing")
        .expect("relc5 added the listing");
    assert_eq!(
        digest_of(&config),
        "783d69c310443d48ebc9c8a943f04fabac67166adf18f5e5f7caa36e5f2f0d9d",
        "section config moved"
    );
    // the tools: every tool but the rule's, the names but its own, the results but its own
    // (and `rdy_schema`, which only the kungfu change moved)
    let mcp = fixture["mcp"].as_object().expect("mcp");
    let without = |v: &Value, keep: &dyn Fn(&str) -> bool| -> Value {
        Value::Object(
            v.as_object()
                .expect("an object")
                .iter()
                .filter(|(k, _)| keep(k))
                .map(|(k, x)| (k.clone(), x.clone()))
                .collect::<Map<String, Value>>(),
        )
    };
    assert_eq!(
        digest_of(&without(&mcp["tools"], &|k| k != "rdy_rule")),
        "63407687adbe2695ceeaef2447414197f3a1a5ff1a170dc8c6cb9e2a1c4d5fa9"
    );
    let names: Vec<&Value> = mcp["tool_names"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| *n != "rdy_rule")
        .collect();
    assert_eq!(
        digest_of(&json!(names)),
        "4d8cdbccd36dafb6e37c9bb020e103aa7c3d288d183ae0bcd53afccb59ffdf46"
    );
    assert_eq!(
        digest_of(&without(&mcp["result_fields"], &|k| {
            !k.starts_with("rdy_rule")
                && k != "rdy_schema"
                && !k.starts_with("rdy_publication_check")
        })),
        "854e7292a55cd4ab9ed0bda185267e218ce9aecb77e2c27223592cd974a2b565"
    );
    for key in [
        "carried_value_refusals",
        "ops_not_probed",
        "refusal_envelope",
        "result_fields_not_captured",
    ] {
        assert!(mcp.contains_key(key), "{key}");
    }
    // the error codes: no code but the kungfu one is new, and no row is
    let errors = &fixture["errors_warnings"];
    let codes: Map<String, Value> = errors["codes"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(k, _)| *k != "W_KUNGFU")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    assert_eq!(
        digest_of(&Value::Object(codes)),
        "f614f31ede7a20703bfe3acf7a2e2605f4713a94a2c2f98e50a5d82a355b716e"
    );
    let rows: Vec<&Value> = errors["error_rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r[0] != "W_KUNGFU")
        .collect();
    assert_eq!(
        digest_of(&json!(rows)),
        "2a6eb32dd02c3ff7bbad7fd1832939658b99c819b18a0018ef1022ccacf12ff4"
    );
    // the command line: every command but `rule`, every probe but the rule's, and the rest
    let cli = &fixture["cli"];
    let others: Vec<&Value> = cli["tree"]["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["name"] != "rule")
        .collect();
    assert_eq!(
        digest_of(&json!(others)),
        "91020aeffb2e3b0d48b86c76abba05c5348f95be31f69f3ce699abee373668e6"
    );
    // relc5 gave the config probe its `levels` and unset rows; the historical digest is of the
    // probe before them
    let mut probes = without(&cli["runtime"]["probes"], &|k| {
        !k.starts_with("rule ") && !k.starts_with("publication check")
    });
    probes["config"] = json!({
        "exits": [0],
        "stdout": [{
            "file": "null|string",
            "key": "string",
            "level": "string",
            "value": "bool|number|string"
        }]
    });
    assert_eq!(
        digest_of(&probes),
        "31041518cd31060f84815d85a99fa6193f25b0d807d8d53fd0a1651c842a9c85"
    );
    // relc4 probes `publication check`, so its exclusion row left `not_probed_commands`; the
    // historical digest is of the runtime with that one row back
    let mut runtime = without(&cli["runtime"], &|k| k != "probes");
    runtime["not_probed_commands"]["publication"] = json!("needs a publication profile");
    // relc1 (Release 1) made the human version text plain, `$.cli.runtime.version_text.text`; the
    // historical digest is of the runtime with that one value in its preview form
    assert_eq!(
        runtime["version_text"]["text"],
        json!("rdy <version>"),
        "the one changed value"
    );
    runtime["version_text"]["text"] = json!("rdy <version> (preview)");
    assert_eq!(
        digest_of(&runtime),
        "8d518aab09e1206edfb24fd47a7dca339e54af78df76cf6d029e5626435f59be"
    );
}

/// relc4 (D-B2AC3): the contract probes the publication check, once as a first publication and
/// once against a baseline, for a profile that hides planned blocks and one that hides none. The
/// shapes hold the hidden lists with their row members, on both surfaces, and the exclusions
/// that said the check could not be probed are gone.
#[test]
fn relc4_contract_probes_populate_hidden_reports() {
    let fixture = read_fixture();
    let row = ["doc", "doc_hidden", "hidden_blocks", "redacted", "title"];
    let list = ["blocks", "docs", "omitted_rows", "rows"];
    let cli = &fixture["cli"]["runtime"]["probes"];
    let mcp = &fixture["mcp"]["result_fields"];
    for (reply, newly) in [
        (&cli["publication check first"]["stdout"], None),
        (
            &cli["publication check against"]["stdout"],
            Some(&cli["publication check against"]["stdout"]["disclosure"]),
        ),
        (&mcp["rdy_publication_check first"]["ok"], None),
        (
            &mcp["rdy_publication_check against"]["ok"],
            Some(&mcp["rdy_publication_check against"]["ok"]["disclosure"]),
        ),
    ] {
        assert_eq!(shape_keys(&reply["hidden_planned"]), list, "{reply}");
        assert_eq!(
            shape_keys(&reply["hidden_planned"]["rows"][0]),
            row,
            "a populated row"
        );
        if let Some(d) = newly {
            let n = &d["newly_hidden_planned"];
            assert_eq!(shape_keys(n), list, "{d}");
            assert_eq!(shape_keys(&n["rows"][0]), row, "a populated row");
        }
    }
    // the first publication has no comparison, and the comparison is an object
    assert_eq!(
        cli["publication check first"]["stdout"]["disclosure"],
        json!("null")
    );
    assert!(cli["publication check against"]["stdout"]["disclosure"].is_object());
    // both surfaces answer with one shape
    assert_eq!(
        cli["publication check first"]["stdout"],
        mcp["rdy_publication_check first"]["ok"]
    );
    assert_eq!(
        cli["publication check against"]["stdout"],
        mcp["rdy_publication_check against"]["ok"]
    );
    // the check is probed, so neither exclusion list holds it
    assert!(
        fixture["cli"]["runtime"]["not_probed_commands"]
            .get("publication")
            .is_none()
    );
    assert!(
        fixture["mcp"]["result_fields_not_captured"]
            .get("rdy_publication_check")
            .is_none()
    );
    // the tools, the tool names and the unprobed operations did not move
    assert!(
        fixture["mcp"]["tool_names"]
            .as_array()
            .unwrap()
            .contains(&json!("rdy_publication_check"))
    );
}

/// relc5 (D-C5955): the contract captures the complete config listing. The config probe holds
/// the allowed levels, and nullable `level`, `file` and `value` from populated and unset rows.
/// The listing holds the defaults and a configured project as text and JSON, with and without
/// origins, with an empty and a populated per-entry group.
#[test]
fn relc5_contract_captures_complete_config_listing() {
    let fixture = read_fixture();
    // $.cli.runtime.probes.config.stdout
    let probe = &fixture["cli"]["runtime"]["probes"]["config"]["stdout"];
    let row = &probe[0];
    assert_eq!(
        shape_keys(row),
        ["file", "key", "level", "levels", "value"],
        "{row}"
    );
    for member in ["file", "level", "value"] {
        let shape = row[member].as_str().unwrap();
        assert!(shape.split('|').any(|s| s == "null"), "{member}: {shape}");
        assert!(shape.split('|').any(|s| s != "null"), "{member}: {shape}");
    }
    assert_eq!(row["levels"], json!(["string"]));
    // $.config.listing
    let listing = &fixture["config"]["listing"];
    let table: Vec<&str> = fixture["config"]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["key"].as_str().unwrap())
        .collect();
    for (scenario, entries) in [("defaults", false), ("configured", true)] {
        let cap = &listing[scenario];
        for member in ["text", "text_show_origin", "json", "json_show_origin"] {
            assert!(!cap[member].is_null(), "{scenario}.{member}");
        }
        // the origin flag changes the text and not the rows
        assert_eq!(cap["json"], cap["json_show_origin"], "{scenario}");
        let rows = cap["json"].as_array().unwrap();
        // every table key has a row, in table order; an entry row sits in its group's slot
        let slot = |key: &str| {
            table
                .iter()
                .position(|t| {
                    key == *t
                        || (["code_paths", "remotes", "hub.installs"].contains(t)
                            && key.starts_with(&format!("{t}.")))
                })
                .unwrap_or_else(|| panic!("{key}"))
        };
        let slots: Vec<usize> = rows
            .iter()
            .map(|r| slot(r["key"].as_str().unwrap()))
            .collect();
        assert!(
            slots.windows(2).all(|w| w[0] <= w[1]),
            "{scenario}: {slots:?}"
        );
        assert!((0..table.len()).all(|i| slots.contains(&i)), "{scenario}");
        for r in rows {
            let want =
                fixture["config"]["keys"][slot(r["key"].as_str().unwrap())]["levels"].clone();
            assert_eq!(r["levels"], want, "{r}");
        }
        // an unset row has null value, level and file; a populated one has a level
        let unset = rows.iter().filter(|r| r["level"].is_null()).count();
        assert!(unset > 0, "{scenario} has unset rows");
        assert!(
            rows.iter()
                .any(|r| r["level"] == "default" && r["file"].is_null()),
            "{scenario} has default rows"
        );
        assert!(
            rows.iter()
                .any(|r| r["level"] == "project" && r["file"].is_string()),
            "{scenario} has configured rows"
        );
        for r in rows.iter().filter(|r| r["level"].is_null()) {
            assert!(r["value"].is_null() && r["file"].is_null(), "{r}");
        }
        let group = |k: &str| {
            rows.iter()
                .filter(|r| r["key"].as_str().unwrap().starts_with(k))
                .count()
        };
        assert_eq!(group("hub.installs"), 1, "an empty group is one unset row");
        assert_eq!(group("code_paths"), 1, "an empty group is one unset row");
        let origin_text = cap["text_show_origin"].as_str().unwrap();
        let text = cap["text"].as_str().unwrap();
        assert!(
            text.contains("hub.installs = unset  # set at: local, user"),
            "{text}"
        );
        assert!(
            origin_text.contains("hub.installs = unset  # unset  # set at: local, user"),
            "{origin_text}"
        );
        assert_eq!(text.lines().count(), rows.len(), "{scenario}");
        // a populated group lists its entry beside no aggregate row
        let remotes = group("remotes");
        if entries {
            assert_eq!(remotes, 1, "one remote");
            assert!(
                rows.iter().any(|r| r["key"] == "remotes.origin"),
                "{rows:?}"
            );
            assert!(
                !rows.iter().any(|r| r["key"] == "remotes"),
                "no aggregate row"
            );
        } else {
            assert!(
                rows.iter()
                    .any(|r| r["key"] == "remotes" && r["level"].is_null())
            );
        }
    }
}
