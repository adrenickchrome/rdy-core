//! ok14: the Release 1 contract inventory (D-TXFHR: the contract surfaces; D-E30RA: Release 1
//! ships after M3a; D-XTWM9; D-69AY3). One machine-readable fixture,
//! `fixtures/contracts/release-1.json`, holds thirteen rows, one for each surface the decision
//! lists. Every row is generated from the code that owns the surface (its registry, parser, schema
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
use rdy_engine::tensions::{Involved, SideRef, tension_id, tension_label, tension_vkey};
use rdy_engine::{Scope, config_levels};
use rdy_model::semantic::{PairItem, pair_key};
use rdy_model::{
    ActorRun, Answer, AnswerAct, Assurance, BlockId, Hash, Provenance, RepoId, ReviewSubject, Role,
    Verdict,
    signature::{StatementKind, StatementSource, statement},
};
use rdy_qa::fixtures::{output_within, project, rdy_bin};
use rdy_schema::{PackSet, TensionKind, TensionSeverity};
use rdy_store::format::{
    FeatureClass, MANIFEST_VERSION, MAX_FEATURES, MAX_MANIFEST_BYTES, REF_STORE_FEATURES,
    REF_UPGRADE, REGISTERED_FEATURES, baseline_features, registered_features, required_features,
};
use rdy_store::{codec, dump, kinds, sync_stream};
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
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

fn fixture_path() -> PathBuf {
    qa_root().join("fixtures/contracts/release-1.json")
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
    for p in paths {
        let s = p.to_string_lossy();
        out = out.replace(s.as_ref(), "<path>");
        if let Ok(c) = p.canonicalize() {
            out = out.replace(c.to_string_lossy().as_ref(), "<path>");
        }
    }
    let port = Regex::new(r"127\.0\.0\.1:[0-9]+").unwrap();
    port.replace_all(&out, "127.0.0.1:<port>").into_owned()
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
    ]
    .into_iter()
    .map(|(n, k)| (n.to_string(), k))
    .collect();
    assert_eq!(
        kind_ids.values().max(),
        Some(&kinds::CHECK_REQUEST),
        "the kind ids end at CHECK_REQUEST"
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
            "object_encoding": "any frame of encoding 1 or more",
            "plan_tension": "a PLAN with a sealed Tension item or Dismissed outcome, or an answer with act Dismiss",
            "view_replay": "a CHANGE whose links.replayed_from is set",
        },
        "encoding_versions": {
            "frame_header_bytes": 37,
            "versioned_flag": codec::VERSIONED,
            "registered": codec::REGISTERED.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
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

/// Runs the release binary in `dir` without a daemon; returns its exit value, stdout and stderr.
fn rdy_in(dir: &Path, env: &[(String, String)], args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(rdy_bin());
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
    let mut ids_seen = ids;
    ids_seen.sort();
    json!({
        "sources": ["rdy checkout", ".rdy/wc", "rdy check", "rdy_engine::wc"],
        "state_directory": ".rdy/wc",
        "state_files": state_names,
        "state_shapes": state,
        "member_shapes": members,
        "checkout_reply": shape(&checkout),
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
    // a view that gives the reviewers a request
    put(&mut r, "a", upd(&batch.spec.c, "C contradicts B."));
    r.close().expect("close");
    rdy_qa::packs::use_pack(dir.path(), "rdy-release", RICH_PACK, "");
    std::fs::write(dir.path().join(".rdy/local.yaml"), "actor: human:qa\n").expect("the person");
    rdy_model::unseed_ids_for_tests();
    Rich { dir, batch }
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
    ("new profile", "needs a publication profile"),
    ("new source", "needs a source file"),
    ("plan defer", "needs an open plan item"),
    ("plan dismiss", "needs an open plan item"),
    ("plan done", "needs an open plan item"),
    (
        "policy",
        "writes a decision and a policy block and proposes them; mcp_policy_init pins its replies",
    ),
    ("publication", "needs a publication profile"),
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
    ("rdy_publication_check", "needs a publication profile"),
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
    let mut w = McpClient::start(dir);
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
        ("rdy_context", json!({"id": a})),
        ("rdy_impact", json!({"id": a})),
        ("rdy_trace", json!({"id": a})),
        ("rdy_search", json!({"query": "holds"})),
        ("rdy_diff", json!({"from": "batch"})),
        ("rdy_query", json!({"filter": "type = claim"})),
        ("rdy_brief", json!({"ids": [a]})),
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
    let mut rv = McpClient::start(dir);
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
    let mut w = McpClient::start(dir);
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
    let mut rv = McpClient::start(dir);
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
    let mut ck = McpClient::start(dir);
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
    let mut rv = McpClient::start(dir);
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
    let mut ck = McpClient::start(dir);
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
    let _ = (doc, a);
}

fn mcp_row() -> Value {
    use rdy_qa::mcp::{McpClient, Reply};
    let rich = rich_project();
    let mut seen = Seen::default();
    mcp_probes(&rich, &mut seen);
    let (d, r) = rdy_qa::fixtures::project();
    drop(r);
    let mut c = McpClient::start(d.path());
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
    let mut rv = McpClient::start(d.path());
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
        "refusal_envelope": refusal,
        "refusal_error_rows": rdy_mcp::tools::ERROR_ROWS.iter().map(|(c, _, _)| *c).collect::<Vec<_>>(),
    })
}

// ---- errors and warnings ----

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
    let pack_keys: Vec<String> = pack_json.as_object().unwrap().keys().cloned().collect();
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
        "validation": {
            "unknown_pack_key": bad_key,
            "unknown_field_kind": bad_field,
            "duplicate_pack_name": duplicate,
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
    "cli/coverage/stdout/kinds",
    "cli/coverage/stdout/similar_kinds",
    "cli/diff/stdout/entries[]/fields",
    "cli/diff/stdout/entries[]/links",
    "cli/diff/stdout/entries[]/repinned",
    "cli/fsck/stdout/open_report/quarantined",
    "cli/fsck/stdout/quarantine",
    "cli/inbox/stdout/entries",
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
    "cli/pending/stdout/not_ready",
    "cli/plan/stdout/notes",
    "cli/propose/stdout/blocking",
    "cli/propose/stdout/conflicts",
    "cli/propose/stdout/consequences",
    "cli/propose/stdout/issues",
    "cli/propose/stdout/plan/items",
    "cli/propose/stdout/tensions",
    "cli/propose/stdout/warnings",
    "cli/remote list/stdout/remotes",
    "cli/remote list/stdout/warnings",
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
    "mcp/rdy_propose/ok/consequences",
    "mcp/rdy_propose/ok/issues",
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
    "mcp/rdy_schema/ok/checks?/agent[]/stages",
    "mcp/rdy_schema/ok/children?",
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
