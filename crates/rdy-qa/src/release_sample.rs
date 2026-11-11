//! The Tally sample: the build list, the builder of the frozen dump, and the bounded check of a
//! frozen dump with an explicit `rdy` binary (`qa-release-sample`).
//!
//! The sample lives in `samples/tally/`: `build.json` (the build list), `tally.rdydump` (the
//! frozen dump) and `freeze.json` (its record). The builder runs once, before publication. The
//! check runs in the ordinary gate and on each native target. It loads the frozen bytes into a new
//! directory, audits the whole history, runs the first session through the real stdio MCP path,
//! reopens the project and verifies the result. It never writes into `samples/`.
//!
//! Every `rdy` this module starts has an isolated home below the work directory, with the user
//! config `actor: human:sample-author` and `signing.sign: never`. Nothing reads or writes the real
//! person layer or a real project. Agent calls name `agent:sample-agent`. These are the only two
//! actors the sample records.
//!
//! # Closed failures
//!
//! Each failure is one line on stderr, ended by LF, and no receipt is written:
//!
//! | Condition | stderr text | Exit |
//! | --- | --- | --- |
//! | Invalid command arguments | `release sample: invalid arguments` | 2 |
//! | Unknown build-list key | `release sample: unknown key <key>` | 1 |
//! | Close unknown build-list key | `release sample: unknown key <key>; did you mean <known>?` | 1 |
//! | Invalid shape or unresolved name | `release sample: invalid build list` | 1 |
//! | Input or output exceeds its bound | `release sample: size limit exceeded` | 1 |
//! | Unsafe baseline history or open work | `release sample: baseline audit failed` | 1 |
//! | Frozen bytes or metadata disagree | `release sample: frozen dump does not verify` | 1 |
//! | Local cycle misses an assertion | `release sample: first session failed` | 1 |
//! | File operation fails | `release sample: file operation failed` | 1 |
//!
//! A refused `rdy` step while the builder writes the baseline is a baseline audit failure. A load
//! or fsck of the frozen dump that fails, or that disagrees with the freeze record, is a frozen
//! dump failure.
//!
//! # The freeze record
//!
//! `freeze.json` has exactly these keys: `name` (`tally.rdydump`), `size` (bytes), `sha256` and
//! `blake3` (lowercase hex of the dump), `build_list_sha256` (of `build.json`),
//! `builder_revision` (the 40-hex commit the builder ran from), `rdy_version` (the builder
//! binary's version), `dump_format` and `store_format` (as `rdy dump --json` reported them),
//! `landing` (the baseline landing), `tag` (the baseline tag) and `ids` (each local name of the
//! build list and the id that rdy gave it).
//!
//! # The receipt
//!
//! A passing check prints one JSON object and LF. Its keys are exactly [`RECEIPT_KEYS`]:
//!
//! - `receipt`: [`RECEIPT_FORMAT`].
//! - `result`: `pass`.
//! - `rdy_version`: the version the checked binary prints.
//! - `platform`: the helper's architecture and operating system, as `<arch>-<os>`.
//! - `binary_sha256`: the SHA-256 of the checked binary.
//! - `dump_name`, `dump_size`, `dump_sha256`, `dump_blake3`: the checked dump.
//! - `freeze_sha256`, `build_list_sha256`: the SHA-256 of the two records.
//! - `baseline_landing`, `baseline_tag`: the frozen baseline.
//! - `session_landing`, `session_tag`: the first session's landing and tag.
//! - `transcript_sha256`: the SHA-256 of the first session's transcript: one JSON line per tool
//!   call, with its tool, arguments and reply, in call order.
//! - `calls`: the number of tool calls in that transcript.
//! - `skipped`: always 0. The check has no optional step.
use crate::mcp::{McpClient, McpFailure, Reply};
use crate::platform::{self, RELEASE_CHILD_DEADLINE, RELEASE_CHILD_OUTPUT_MAX};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The person who approves and tags.
pub const HUMAN_ACTOR: &str = "human:sample-author";
/// The agent who writes and relays.
pub const AGENT_ACTOR: &str = "agent:sample-agent";
/// The most bytes in a build list.
pub const BUILD_LIST_MAX: usize = 1_048_576;
/// The most folders, docs and blocks a build list lists, together.
pub const NODES_MAX: usize = 128;
/// The deepest nesting of arrays and objects in a build list.
pub const DEPTH_MAX: usize = 32;
/// The most bytes in one string of a build list, keys included.
pub const STRING_MAX: usize = 65_536;
/// The most bytes in a dump. The release candidate has the same bound.
pub const DUMP_MAX: u64 = 16_777_216;
/// The most bytes in a freeze record or a project config file.
pub const RECORD_MAX: u64 = 65_536;
/// The most bytes in a checked binary.
pub const BINARY_MAX: u64 = 536_870_912;
/// The most bytes one MCP session may write, stdout and stderr together.
pub const SESSION_OUTPUT_MAX: usize = 8_388_608;
/// The file name of the frozen dump.
pub const DUMP_NAME: &str = "tally.rdydump";
/// The format of the build list.
pub const BUILD_LIST_FORMAT: &str = "tally-build-list/1";
/// The format of the receipt.
pub const RECEIPT_FORMAT: &str = "rdy-release-sample/1";
/// The keys of the receipt.
pub const RECEIPT_KEYS: [&str; 18] = [
    "receipt",
    "result",
    "rdy_version",
    "platform",
    "binary_sha256",
    "dump_name",
    "dump_size",
    "dump_sha256",
    "dump_blake3",
    "freeze_sha256",
    "build_list_sha256",
    "baseline_landing",
    "baseline_tag",
    "session_landing",
    "session_tag",
    "transcript_sha256",
    "calls",
    "skipped",
];

/// The user config of every `rdy` this module starts.
const USER_CONFIG: &str = "actor: human:sample-author\nsigning:\n  sign: never\n";
/// The run of the audit's read calls.
const AUDIT_RUN: &str = "baseline-audit";
/// The run of the first session.
const SESSION_RUN: &str = "first-session";
/// The run of the reopened session.
const REOPEN_RUN: &str = "reopen";
/// The session reference of every relayed answer.
const SESSION_REF: &str = "sample";

/// Text that never stands in the dump: places, hosts, addresses, keys and the builder's own
/// session state.
const FORBIDDEN: [&str; 23] = [
    "/Users/",
    "/home/",
    "/private/",
    "/tmp/",
    "/var/",
    "/root/",
    "/etc/",
    ":\\",
    "\\Users\\",
    "rdyqa",
    "localhost",
    "127.0.0.1",
    "http://",
    "https://",
    "file:/",
    "ssh-ed25519",
    "ssh-rsa",
    "ecdsa-sha2",
    "BEGIN ",
    "PRIVATE KEY",
    "inbox/",
    "kf-",
    ".local",
];

/// Why the helper failed. Each failure has one closed message and exit code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SampleFailure {
    InvalidArguments,
    UnknownKey { key: String, known: Option<String> },
    InvalidBuildList,
    SizeLimit,
    BaselineAudit,
    FrozenDump,
    FirstSession,
    File,
}

impl SampleFailure {
    /// The stderr text, without its LF.
    pub fn message(&self) -> String {
        match self {
            SampleFailure::InvalidArguments => "release sample: invalid arguments".into(),
            SampleFailure::UnknownKey { key, known: None } => {
                format!("release sample: unknown key {key}")
            }
            SampleFailure::UnknownKey {
                key,
                known: Some(k),
            } => format!("release sample: unknown key {key}; did you mean {k}?"),
            SampleFailure::InvalidBuildList => "release sample: invalid build list".into(),
            SampleFailure::SizeLimit => "release sample: size limit exceeded".into(),
            SampleFailure::BaselineAudit => "release sample: baseline audit failed".into(),
            SampleFailure::FrozenDump => "release sample: frozen dump does not verify".into(),
            SampleFailure::FirstSession => "release sample: first session failed".into(),
            SampleFailure::File => "release sample: file operation failed".into(),
        }
    }

    /// The exit code: 2 for invalid arguments, else 1.
    pub fn exit_code(&self) -> u8 {
        match self {
            SampleFailure::InvalidArguments => 2,
            _ => 1,
        }
    }
}

type Result<T> = std::result::Result<T, SampleFailure>;

// ---------------------------------------------------------------- bytes and files

/// Lowercase hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 of `bytes`, in lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// The BLAKE3 of `bytes`, in lowercase hex.
pub fn blake3_hex(bytes: &[u8]) -> String {
    rdy_model::Hash::of(bytes).to_string()
}

/// The regular file at `path`, at most `max` bytes, checked before it is read.
fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(|_| SampleFailure::File)?;
    let meta = file.metadata().map_err(|_| SampleFailure::File)?;
    if !meta.is_file() {
        return Err(SampleFailure::File);
    }
    if meta.len() > max {
        return Err(SampleFailure::SizeLimit);
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| SampleFailure::File)?;
    if bytes.len() as u64 > max {
        return Err(SampleFailure::SizeLimit);
    }
    Ok(bytes)
}

/// Writes `bytes` to the new file `path`; an existing file is a failure.
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| SampleFailure::File)?;
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .map_err(|_| SampleFailure::File)
}

/// Whether `s` is `n` lowercase hex digits.
fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

// ---------------------------------------------------------------- the build list

const TOP_KEYS: &[&str] = &["format", "project", "baseline", "folders", "first_session"];
const PROJECT_KEYS: &[&str] = &["name", "description", "pack", "config"];
const CONFIG_KEYS: &[&str] = &["decisions.home"];
const BASELINE_KEYS: &[&str] = &[
    "view",
    "write_message",
    "write_implements",
    "proposal_title",
    "proposal_summary",
    "approval",
    "tag",
];
const RELAY_KEYS: &[&str] = &["question", "evidence"];
const TAG_KEYS: &[&str] = &["version", "message", "question", "evidence"];
const FOLDER_KEYS: &[&str] = &["name", "fields", "docs"];
const FOLDER_FIELDS: &[&str] = &["name", "title", "description"];
const DOC_KEYS: &[&str] = &["name", "fields", "blocks"];
const DOC_FIELDS: &[&str] = &["name", "title", "doc_type", "lifecycle"];
const BLOCK_KEYS: &[&str] = &[
    "name", "type", "fields", "body", "links", "blocks", "decision",
];
const BLOCK_FIELDS: &[&str] = &["name", "aliases", "title", "priority", "steps"];
/// The block fields that hold a list of strings; the others hold one string.
const LIST_FIELDS: &[&str] = &["aliases", "steps", "options", "affects"];
const LINK_KEYS: &[&str] = &["decided_by", "describes", "implements"];
const DECISION_KEYS: &[&str] = &[
    "title",
    "statement",
    "why",
    "options",
    "kind",
    "reversibility",
    "affects",
    "body",
    "recommend",
    "recommend_reason",
];
const FIRST_KEYS: &[&str] = &[
    "ask",
    "search",
    "confirm_reason",
    "supersede",
    "fix",
    "proposal",
    "approval",
    "tag",
];
const SUPERSEDE_KEYS: &[&str] = &["name", "old", "decision"];
const FIX_KEYS: &[&str] = &["implements", "ops"];
const OP_KEYS: &[&str] = &["op", "id", "fields", "body", "links"];
const PROPOSAL_KEYS: &[&str] = &["title", "summary"];
const BLOCK_TYPES: &[&str] = &[
    "prose",
    "term",
    "section",
    "component",
    "decision",
    "requirement",
    "flow",
];

/// A relayed answer of the simulated person: the question and the person's words.
#[derive(Clone, Debug)]
pub struct Relay {
    pub question: String,
    pub evidence: String,
}

/// A tag the person asks for.
#[derive(Clone, Debug)]
pub struct TagSpec {
    pub version: String,
    pub message: String,
    pub question: String,
    pub evidence: String,
}

/// One listed block, in document order.
#[derive(Clone, Debug)]
pub struct BlockSpec {
    pub name: Option<String>,
    pub block_type: String,
    pub fields: Map<String, Value>,
    pub body: Option<String>,
    pub links: BTreeMap<String, Vec<String>>,
    pub blocks: Vec<BlockSpec>,
    /// The `rdy_decide` arguments of a decision, without its parent.
    pub decision: Option<Map<String, Value>>,
}

#[derive(Clone, Debug)]
pub struct DocSpec {
    pub name: String,
    pub fields: Map<String, Value>,
    pub blocks: Vec<BlockSpec>,
}

#[derive(Clone, Debug)]
pub struct FolderSpec {
    pub name: String,
    pub fields: Map<String, Value>,
    pub docs: Vec<DocSpec>,
}

/// A validated build list. Every key is known, every bound holds and every local name resolves.
#[derive(Clone, Debug)]
pub struct BuildList {
    pub text: String,
    pub sha256: String,
    pub name: String,
    pub description: String,
    pub pack: String,
    pub config: Vec<(String, String)>,
    pub view: String,
    pub write_message: String,
    pub write_implements: String,
    pub proposal_title: String,
    pub proposal_summary: String,
    pub approval: Relay,
    pub tag: TagSpec,
    pub folders: Vec<FolderSpec>,
    pub search: String,
    pub confirm_reason: String,
    pub supersede_name: String,
    pub supersede_old: String,
    pub supersede_decision: Map<String, Value>,
    pub fix_implements: String,
    pub fix_ops: Vec<Value>,
    pub session_title: String,
    pub session_summary: String,
    pub session_approval: Relay,
    pub session_tag: TagSpec,
    baseline_names: Vec<String>,
    decisions: BTreeSet<String>,
    /// Each named block's parent local name and type.
    parents: BTreeMap<String, (String, String)>,
}

impl BuildList {
    /// The folders, docs and blocks the list holds.
    pub fn counts(&self) -> (usize, usize, usize) {
        fn blocks(b: &[BlockSpec]) -> usize {
            b.iter().map(|x| 1 + blocks(&x.blocks)).sum()
        }
        let docs: Vec<&DocSpec> = self.folders.iter().flat_map(|f| &f.docs).collect();
        let n = docs.iter().map(|d| blocks(&d.blocks)).sum();
        (self.folders.len(), docs.len(), n)
    }

    /// Every block of the project: the folders and docs are blocks too.
    pub fn total_blocks(&self) -> usize {
        let (f, d, b) = self.counts();
        f + d + b
    }

    /// The local names of the baseline, in list order.
    pub fn names(&self) -> Vec<String> {
        self.baseline_names.clone()
    }

    /// The parent local name and type of the named block `name`.
    fn parent_of(&self, name: &str) -> Option<&(String, String)> {
        self.parents.get(name)
    }

    /// The type of the named block `name`.
    fn type_of(&self, name: &str) -> Option<&str> {
        self.parents.get(name).map(|(_, t)| t.as_str())
    }
}

/// The `{{$name` references in a text.
fn refs_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\{\{(\$[^|}]*)").expect("the reference pattern"))
}

/// Whether `s` is a local name: `$`, a letter, then up to 63 letters, digits or `_`.
fn is_local(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('$') else {
        return false;
    };
    !rest.is_empty()
        && rest.len() <= 64
        && rest.as_bytes()[0].is_ascii_alphabetic()
        && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Checks the bounds of `v` at `depth` (the top object is depth 1).
fn bounds(v: &Value, depth: usize) -> Result<()> {
    match v {
        Value::String(s) if s.len() > STRING_MAX => Err(SampleFailure::SizeLimit),
        Value::Array(a) => {
            if depth > DEPTH_MAX {
                return Err(SampleFailure::SizeLimit);
            }
            a.iter().try_for_each(|x| bounds(x, depth + 1))
        }
        Value::Object(m) => {
            if depth > DEPTH_MAX {
                return Err(SampleFailure::SizeLimit);
            }
            for (k, x) in m {
                if k.len() > STRING_MAX {
                    return Err(SampleFailure::SizeLimit);
                }
                bounds(x, depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// `v` as an object whose keys are all in `known`.
fn object<'a>(v: &'a Value, known: &[&str]) -> Result<&'a Map<String, Value>> {
    let m = v.as_object().ok_or(SampleFailure::InvalidBuildList)?;
    for k in m.keys() {
        if !known.contains(&k.as_str()) {
            return Err(SampleFailure::UnknownKey {
                key: k.clone(),
                known: rdy_engine::did_you_mean::nearest(k, known).map(str::to_string),
            });
        }
    }
    Ok(m)
}

/// The nonempty string at `key` of `m`.
fn text(m: &Map<String, Value>, key: &str) -> Result<String> {
    match m.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        _ => Err(SampleFailure::InvalidBuildList),
    }
}

/// The nonempty strings of the array `v`.
fn texts(v: &Value) -> Result<Vec<String>> {
    let a = v.as_array().ok_or(SampleFailure::InvalidBuildList)?;
    a.iter()
        .map(|x| match x {
            Value::String(s) if !s.is_empty() => Ok(s.clone()),
            _ => Err(SampleFailure::InvalidBuildList),
        })
        .collect()
}

/// A field map of closed `known` keys: a list field holds nonempty strings, any other field one
/// nonempty string.
fn fields(v: Option<&Value>, known: &[&str]) -> Result<Map<String, Value>> {
    let Some(v) = v else {
        return Ok(Map::new());
    };
    let m = object(v, known)?;
    for (k, x) in m {
        if LIST_FIELDS.contains(&k.as_str()) {
            texts(x)?;
        } else if !matches!(x, Value::String(s) if !s.is_empty()) {
            return Err(SampleFailure::InvalidBuildList);
        }
    }
    Ok(m.clone())
}

/// A link map of closed kinds, each a list of local names.
fn links(v: Option<&Value>) -> Result<BTreeMap<String, Vec<String>>> {
    let Some(v) = v else {
        return Ok(BTreeMap::new());
    };
    let m = object(v, LINK_KEYS)?;
    let mut out = BTreeMap::new();
    for (k, x) in m {
        let names = texts(x)?;
        if names.is_empty() || !names.iter().all(|n| is_local(n)) {
            return Err(SampleFailure::InvalidBuildList);
        }
        out.insert(k.clone(), names);
    }
    Ok(out)
}

fn relay(v: Option<&Value>) -> Result<Relay> {
    let m = object(v.ok_or(SampleFailure::InvalidBuildList)?, RELAY_KEYS)?;
    Ok(Relay {
        question: text(m, "question")?,
        evidence: text(m, "evidence")?,
    })
}

fn tag_spec(v: Option<&Value>) -> Result<TagSpec> {
    let m = object(v.ok_or(SampleFailure::InvalidBuildList)?, TAG_KEYS)?;
    Ok(TagSpec {
        version: text(m, "version")?,
        message: text(m, "message")?,
        question: text(m, "question")?,
        evidence: text(m, "evidence")?,
    })
}

/// Every string inside `v`.
fn strings_in(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| strings_in(x, out)),
        Value::Object(m) => m.values().for_each(|x| strings_in(x, out)),
        _ => {}
    }
}

/// The local names that the texts of `v` reference with `{{$name`; a malformed one is invalid.
fn text_refs(v: &Value, out: &mut Vec<String>) -> Result<()> {
    let mut all = Vec::new();
    strings_in(v, &mut all);
    for s in all {
        for c in refs_regex().captures_iter(&s) {
            let name = c[1].to_string();
            if !is_local(&name) {
                return Err(SampleFailure::InvalidBuildList);
            }
            out.push(name);
        }
    }
    Ok(())
}

/// The state of one walk of the build list.
#[derive(Default)]
struct Walk {
    nodes: usize,
    names: Vec<String>,
    defined: BTreeSet<String>,
    refs: Vec<String>,
    decisions: BTreeSet<String>,
    parents: BTreeMap<String, (String, String)>,
}

impl Walk {
    fn count(&mut self) -> Result<()> {
        self.nodes += 1;
        if self.nodes > NODES_MAX {
            return Err(SampleFailure::SizeLimit);
        }
        Ok(())
    }

    fn define(&mut self, name: &str, parent: &str, block_type: &str) -> Result<()> {
        if !is_local(name) || !self.defined.insert(name.to_string()) {
            return Err(SampleFailure::InvalidBuildList);
        }
        self.names.push(name.to_string());
        self.parents
            .insert(name.into(), (parent.into(), block_type.into()));
        Ok(())
    }

    fn block(&mut self, v: &Value, parent: &str) -> Result<BlockSpec> {
        let m = object(v, BLOCK_KEYS)?;
        self.count()?;
        let block_type = text(m, "type")?;
        if !BLOCK_TYPES.contains(&block_type.as_str()) {
            return Err(SampleFailure::InvalidBuildList);
        }
        let name = match m.get("name") {
            None => None,
            Some(_) => Some(text(m, "name")?),
        };
        if let Some(n) = &name {
            self.define(n, parent, &block_type)?;
        }
        let body = match m.get("body") {
            None => None,
            Some(_) => Some(text(m, "body")?),
        };
        let fields = fields(m.get("fields"), BLOCK_FIELDS)?;
        let links = links(m.get("links"))?;
        let decision = match m.get("decision") {
            None => None,
            Some(d) => Some(decision_args(d)?),
        };
        let is_decision = block_type == "decision";
        if is_decision != decision.is_some()
            || (is_decision
                && (name.is_none()
                    || body.is_some()
                    || !fields.is_empty()
                    || !links.is_empty()
                    || m.contains_key("blocks")))
        {
            return Err(SampleFailure::InvalidBuildList);
        }
        if let (true, Some(n)) = (is_decision, &name) {
            self.decisions.insert(n.clone());
        }
        let mut blocks = Vec::new();
        if let Some(children) = m.get("blocks") {
            let Some(n) = name.as_ref().filter(|_| block_type == "section") else {
                return Err(SampleFailure::InvalidBuildList);
            };
            for c in children.as_array().ok_or(SampleFailure::InvalidBuildList)? {
                blocks.push(self.block(c, n)?);
            }
        }
        for names in links.values() {
            self.refs.extend(names.iter().cloned());
        }
        if let Some(d) = &decision {
            for a in d.get("affects").map(texts).transpose()?.unwrap_or_default() {
                self.refs.push(a);
            }
        }
        text_refs(&Value::Object(m.clone()), &mut self.refs)?;
        Ok(BlockSpec {
            name,
            block_type,
            fields,
            body,
            links,
            blocks,
            decision,
        })
    }
}

/// The `rdy_decide` arguments of a decision: closed keys, nonempty strings and string lists.
fn decision_args(v: &Value) -> Result<Map<String, Value>> {
    let m = fields(Some(v), DECISION_KEYS)?;
    text(&m, "title")?;
    text(&m, "statement")?;
    if let Some(a) = m.get("affects")
        && !texts(a)?.iter().all(|n| is_local(n))
    {
        return Err(SampleFailure::InvalidBuildList);
    }
    Ok(m)
}

/// Validates the build list `bytes`: its size, then its JSON, then its bounds, then every key,
/// then its shape and its local names. Nothing is written or run.
pub fn parse_build_list(bytes: &[u8]) -> Result<BuildList> {
    if bytes.len() > BUILD_LIST_MAX {
        return Err(SampleFailure::SizeLimit);
    }
    let text_all = std::str::from_utf8(bytes).map_err(|_| SampleFailure::InvalidBuildList)?;
    let v: Value = serde_json::from_str(text_all).map_err(|_| SampleFailure::InvalidBuildList)?;
    bounds(&v, 1)?;
    let top = object(&v, TOP_KEYS)?;
    if text(top, "format")? != BUILD_LIST_FORMAT {
        return Err(SampleFailure::InvalidBuildList);
    }
    let project = object(
        top.get("project").ok_or(SampleFailure::InvalidBuildList)?,
        PROJECT_KEYS,
    )?;
    let mut config = Vec::new();
    if let Some(c) = project.get("config") {
        for (k, x) in object(c, CONFIG_KEYS)? {
            match x {
                Value::String(s) if !s.is_empty() => config.push((k.clone(), s.clone())),
                _ => return Err(SampleFailure::InvalidBuildList),
            }
        }
    }
    let baseline = object(
        top.get("baseline").ok_or(SampleFailure::InvalidBuildList)?,
        BASELINE_KEYS,
    )?;
    let mut walk = Walk::default();
    let mut folders = Vec::new();
    for f in top
        .get("folders")
        .and_then(Value::as_array)
        .ok_or(SampleFailure::InvalidBuildList)?
    {
        let fm = object(f, FOLDER_KEYS)?;
        walk.count()?;
        let fname = text(fm, "name")?;
        walk.define(&fname, "", "folder")?;
        let ffields = fields(fm.get("fields"), FOLDER_FIELDS)?;
        text(&ffields, "name")?;
        let mut docs = Vec::new();
        for d in fm
            .get("docs")
            .and_then(Value::as_array)
            .ok_or(SampleFailure::InvalidBuildList)?
        {
            let dm = object(d, DOC_KEYS)?;
            walk.count()?;
            let dname = text(dm, "name")?;
            walk.define(&dname, &fname, "doc")?;
            let dfields = fields(dm.get("fields"), DOC_FIELDS)?;
            text(&dfields, "name")?;
            let mut blocks = Vec::new();
            for b in dm
                .get("blocks")
                .and_then(Value::as_array)
                .ok_or(SampleFailure::InvalidBuildList)?
            {
                blocks.push(walk.block(b, &dname)?);
            }
            docs.push(DocSpec {
                name: dname,
                fields: dfields,
                blocks,
            });
        }
        folders.push(FolderSpec {
            name: fname,
            fields: ffields,
            docs,
        });
    }
    let write_implements = text(baseline, "write_implements")?;
    if !walk.decisions.contains(&write_implements)
        || !walk.refs.iter().all(|r| walk.defined.contains(r))
    {
        return Err(SampleFailure::InvalidBuildList);
    }

    let first = object(
        top.get("first_session")
            .ok_or(SampleFailure::InvalidBuildList)?,
        FIRST_KEYS,
    )?;
    let sup = object(
        first
            .get("supersede")
            .ok_or(SampleFailure::InvalidBuildList)?,
        SUPERSEDE_KEYS,
    )?;
    let supersede_name = text(sup, "name")?;
    let supersede_old = text(sup, "old")?;
    let supersede_decision =
        decision_args(sup.get("decision").ok_or(SampleFailure::InvalidBuildList)?)?;
    if !is_local(&supersede_name)
        || walk.defined.contains(&supersede_name)
        || !walk.decisions.contains(&supersede_old)
    {
        return Err(SampleFailure::InvalidBuildList);
    }
    let fix = object(
        first.get("fix").ok_or(SampleFailure::InvalidBuildList)?,
        FIX_KEYS,
    )?;
    let fix_implements = text(fix, "implements")?;
    let mut fix_ops = Vec::new();
    let mut targets = BTreeSet::new();
    let mut session_refs = Vec::new();
    for op in fix
        .get("ops")
        .and_then(Value::as_array)
        .ok_or(SampleFailure::InvalidBuildList)?
    {
        let om = object(op, OP_KEYS)?;
        let id = text(om, "id")?;
        if text(om, "op")? != "update"
            || walk.decisions.contains(&id)
            || !walk.parents.contains_key(&id)
            || !targets.insert(id.clone())
        {
            return Err(SampleFailure::InvalidBuildList);
        }
        fields(om.get("fields"), BLOCK_FIELDS)?;
        if om.contains_key("body") {
            text(om, "body")?;
        }
        for names in links(om.get("links"))?.values() {
            session_refs.extend(names.iter().cloned());
        }
        text_refs(op, &mut session_refs)?;
        fix_ops.push(op.clone());
    }
    // the fix writes exactly the blocks decided by the superseded decision
    let dependents: BTreeSet<String> = all_blocks(&folders)
        .into_iter()
        .filter(|b| {
            b.links
                .get("decided_by")
                .is_some_and(|d| d.contains(&supersede_old))
        })
        .filter_map(|b| b.name.clone())
        .collect();
    text_refs(
        &Value::Object(supersede_decision.clone()),
        &mut session_refs,
    )?;
    let in_session = |r: &String| walk.defined.contains(r) || *r == supersede_name;
    if fix_implements != supersede_name
        || targets.is_empty()
        || targets != dependents
        || !session_refs.iter().all(in_session)
    {
        return Err(SampleFailure::InvalidBuildList);
    }
    let proposal = object(
        first
            .get("proposal")
            .ok_or(SampleFailure::InvalidBuildList)?,
        PROPOSAL_KEYS,
    )?;
    text(first, "ask")?;
    Ok(BuildList {
        text: text_all.to_string(),
        sha256: sha256_hex(bytes),
        name: text(project, "name")?,
        description: text(project, "description")?,
        pack: text(project, "pack")?,
        config,
        view: text(baseline, "view")?,
        write_message: text(baseline, "write_message")?,
        write_implements,
        proposal_title: text(baseline, "proposal_title")?,
        proposal_summary: text(baseline, "proposal_summary")?,
        approval: relay(baseline.get("approval"))?,
        tag: tag_spec(baseline.get("tag"))?,
        folders,
        search: text(first, "search")?,
        confirm_reason: text(first, "confirm_reason")?,
        supersede_name,
        supersede_old,
        supersede_decision,
        fix_implements,
        fix_ops,
        session_title: text(proposal, "title")?,
        session_summary: text(proposal, "summary")?,
        session_approval: relay(first.get("approval"))?,
        session_tag: tag_spec(first.get("tag"))?,
        baseline_names: walk.names,
        decisions: walk.decisions,
        parents: walk.parents,
    })
}

/// Every listed block, in document order.
fn all_blocks(folders: &[FolderSpec]) -> Vec<&BlockSpec> {
    fn add<'a>(b: &'a [BlockSpec], out: &mut Vec<&'a BlockSpec>) {
        for x in b {
            out.push(x);
            add(&x.blocks, out);
        }
    }
    let mut out = Vec::new();
    for d in folders.iter().flat_map(|f| &f.docs) {
        add(&d.blocks, &mut out);
    }
    out
}

/// Reads and validates the build list at `path`.
pub fn read_build_list(path: &Path) -> Result<BuildList> {
    parse_build_list(&read_bounded(path, BUILD_LIST_MAX as u64)?)
}

/// The project config file the build list gives: `pack`, `name`, `description`, then each
/// config key, a dotted key nested one level.
pub fn render_config(list: &BuildList) -> Result<String> {
    use serde_yaml_ng::{Mapping, Value as Y};
    let mut m = Mapping::new();
    m.insert(Y::from("pack"), Y::from(list.pack.as_str()));
    m.insert(Y::from("name"), Y::from(list.name.as_str()));
    m.insert(Y::from("description"), Y::from(list.description.as_str()));
    for (k, v) in &list.config {
        let (outer, inner) = k.split_once('.').ok_or(SampleFailure::InvalidBuildList)?;
        let entry = m
            .entry(Y::from(outer))
            .or_insert_with(|| Y::Mapping(Mapping::new()));
        let Y::Mapping(sub) = entry else {
            return Err(SampleFailure::InvalidBuildList);
        };
        sub.insert(Y::from(inner), Y::from(v.as_str()));
    }
    serde_yaml_ng::to_string(&Y::Mapping(m)).map_err(|_| SampleFailure::InvalidBuildList)
}

// ---------------------------------------------------------------- the freeze record

/// The freeze record of a frozen dump (`freeze.json`); see the module documentation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Freeze {
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub blake3: String,
    pub build_list_sha256: String,
    pub builder_revision: String,
    pub rdy_version: String,
    pub dump_format: u64,
    pub store_format: Value,
    pub landing: String,
    pub tag: String,
    pub ids: BTreeMap<String, String>,
}

/// Reads the freeze record at `path`. A record that does not parse, or a fact out of its form,
/// is a frozen dump failure.
pub fn read_freeze(path: &Path) -> Result<(Freeze, String)> {
    let bytes = read_bounded(path, RECORD_MAX)?;
    let f: Freeze = serde_json::from_slice(&bytes).map_err(|_| SampleFailure::FrozenDump)?;
    let ok = f.name == DUMP_NAME
        && (1..=DUMP_MAX).contains(&f.size)
        && is_hex(&f.sha256, 64)
        && is_hex(&f.blake3, 64)
        && is_hex(&f.build_list_sha256, 64)
        && is_hex(&f.builder_revision, 40)
        && is_hex(&f.landing, 64)
        && !f.rdy_version.is_empty()
        && !f.tag.is_empty()
        && f.ids.iter().all(|(k, v)| is_local(k) && !v.is_empty());
    if !ok {
        return Err(SampleFailure::FrozenDump);
    }
    Ok((f, sha256_hex(&bytes)))
}

/// Whether `dump` is the dump `freeze` records, frozen from `list`.
pub fn verify_frozen(dump: &[u8], freeze: &Freeze, list: &BuildList) -> Result<()> {
    let ids: BTreeSet<&String> = freeze.ids.keys().collect();
    let names: BTreeSet<&String> = list.baseline_names.iter().collect();
    if dump.len() as u64 != freeze.size
        || sha256_hex(dump) != freeze.sha256
        || blake3_hex(dump) != freeze.blake3
        || list.sha256 != freeze.build_list_sha256
        || ids != names
        || list.tag.version != freeze.tag
    {
        return Err(SampleFailure::FrozenDump);
    }
    Ok(())
}

// ---------------------------------------------------------------- processes

/// The isolated places of one project: its home and its config place.
#[derive(Clone, Debug)]
struct Person {
    home: PathBuf,
    config: PathBuf,
}

impl Person {
    /// Makes `dir`'s home and the user config.
    fn make(dir: &Path) -> Result<Person> {
        let home = dir.join("home");
        let config = home.join(".config");
        let rdy = config.join("rdy");
        std::fs::create_dir_all(&rdy).map_err(|_| SampleFailure::File)?;
        write_new(&rdy.join("config.yaml"), USER_CONFIG.as_bytes())?;
        Ok(Person { home, config })
    }

    /// `rdy` with this person and no session variables.
    fn command(&self, rdy: &Path) -> Command {
        let mut cmd = Command::new(rdy);
        crate::fixtures::isolate_person(&mut cmd, &self.home, &self.config);
        cmd.env_remove("RDY_ACTOR")
            .env_remove("RDY_RUN")
            .env_remove("RDY_NO_DAEMON");
        cmd
    }
}

/// Runs `rdy` with `args` as `person`, bounded: its JSON stdout. A nonzero exit or an output
/// that is not JSON is `stage`.
fn run_json(
    rdy: &Path,
    person: &Person,
    args: &[&std::ffi::OsStr],
    stage: SampleFailure,
) -> Result<Value> {
    let mut cmd = person.command(rdy);
    cmd.args(args);
    let out = platform::run_bounded(cmd, None, RELEASE_CHILD_OUTPUT_MAX, RELEASE_CHILD_DEADLINE)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::InvalidData => SampleFailure::SizeLimit,
            _ => stage.clone(),
        })?;
    if !out.status.success() {
        return Err(stage);
    }
    serde_json::from_slice(&out.stdout).map_err(|_| stage)
}

/// The version `rdy --version` prints: the second word of its first line.
fn rdy_version(rdy: &Path, person: &Person, stage: SampleFailure) -> Result<String> {
    let mut cmd = person.command(rdy);
    cmd.arg("--version");
    let out = platform::run_bounded(cmd, None, RELEASE_CHILD_OUTPUT_MAX, RELEASE_CHILD_DEADLINE)
        .map_err(|_| stage.clone())?;
    let text = String::from_utf8(out.stdout).map_err(|_| stage.clone())?;
    match text
        .lines()
        .next()
        .map(|l| l.split(' ').collect::<Vec<_>>())
    {
        Some(w) if out.status.success() && w.len() == 2 && w[0] == "rdy" && !w[1].is_empty() => {
            Ok(w[1].to_string())
        }
        _ => Err(stage),
    }
}

// ---------------------------------------------------------------- MCP sessions

/// The kungfu value of each tool, from the instructions' texts.
fn kungfu_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"pass kungfu: "(kf-[0-9a-f]+)"\. Tools: ([a-z_, ]+)\."#)
            .expect("the kungfu pattern")
    })
}

/// One bounded MCP session of the `rdy` under check, with its transcript.
struct Session {
    client: McpClient,
    /// The session stated on each request, when it has no `rdy_session`.
    meta: Option<Value>,
    kungfu: BTreeMap<String, String>,
    transcript: Sha256,
    calls: u64,
    /// The failure of a refused step.
    stage: SampleFailure,
}

fn mcp_failure(f: McpFailure, stage: &SampleFailure) -> SampleFailure {
    match f {
        McpFailure::OutputLimit => SampleFailure::SizeLimit,
        _ => stage.clone(),
    }
}

impl Session {
    /// `rdy mcp --no-daemon --project <project>` as `person`.
    fn start(
        rdy: &Path,
        person: &Person,
        project: &Path,
        meta: Option<Value>,
        stage: SampleFailure,
    ) -> Result<Session> {
        let mut cmd = person.command(rdy);
        cmd.args(["mcp", "--no-daemon", "--project"]).arg(project);
        let client = McpClient::try_launch_bounded(cmd, SESSION_OUTPUT_MAX)
            .map_err(|f| mcp_failure(f, &stage))?;
        Ok(Session {
            client,
            meta,
            kungfu: BTreeMap::new(),
            transcript: Sha256::new(),
            calls: 0,
            stage,
        })
    }

    /// Calls `tool` with `args` and the tool's kungfu value: the reply, or the tool's error.
    fn reply(&mut self, tool: &str, mut args: Value) -> Result<std::result::Result<Value, Value>> {
        if let (Some(k), Some(m)) = (self.kungfu.get(tool), args.as_object_mut()) {
            m.insert("kungfu".into(), json!(k));
        }
        let r = match &self.meta {
            Some(meta) => self.client.try_call_meta(tool, args.clone(), &meta.clone()),
            None => self.client.try_call(tool, args.clone()),
        }
        .map_err(|f| mcp_failure(f, &self.stage))?;
        let (ok, v) = match r {
            Reply::Ok(v) => (true, v),
            Reply::ToolError(v) | Reply::RpcError(v) => (false, v),
        };
        let line = json!({"tool": tool, "args": args, "reply": v});
        self.transcript.update(line.to_string().as_bytes());
        self.transcript.update(b"\n");
        self.calls += 1;
        Ok(if ok { Ok(v) } else { Err(v) })
    }

    /// Calls `tool`; its error is the session's stage failure.
    fn call(&mut self, tool: &str, args: Value) -> Result<Value> {
        self.reply(tool, args)?.map_err(|_| self.stage.clone())
    }

    /// The required instructions, then every built-in ruleset; learns each tool's kungfu.
    fn instructions(&mut self) -> Result<()> {
        for args in [json!({}), json!({"full": true})] {
            let v = self.call("rdy_instructions", args)?;
            for i in v["instructions"].as_array().into_iter().flatten() {
                let t = i["text"].as_str().unwrap_or("");
                for c in kungfu_regex().captures_iter(t) {
                    for tool in c[2].split(',') {
                        self.kungfu
                            .insert(tool.trim().to_string(), c[1].to_string());
                    }
                }
            }
        }
        Ok(())
    }

    /// Ends the session: the server must exit 0. The transcript digest and the call count.
    fn finish(self) -> Result<(String, u64)> {
        let stage = self.stage.clone();
        let t = self
            .client
            .try_finish(crate::mcp::DEADLINE)
            .map_err(|f| mcp_failure(f, &stage))?;
        if t.code != Some(0) {
            return Err(stage);
        }
        Ok((hex(&self.transcript.finalize()), self.calls))
    }
}

/// The session stated on each request for `run`.
fn meta(run: &str) -> Value {
    json!({"rdy/actor": AGENT_ACTOR, "rdy/run": run})
}

/// `s` with each `{{$name` replaced by the id `ids` gives it.
fn resolve_text(s: &str, ids: &BTreeMap<String, String>) -> String {
    refs_regex()
        .replace_all(s, |c: &regex::Captures| match ids.get(&c[1]) {
            Some(id) => format!("{{{{{id}"),
            None => c[0].to_string(),
        })
        .into_owned()
}

/// `v` with every text's references resolved by `ids`.
fn resolve_value(v: &Value, ids: &BTreeMap<String, String>) -> Value {
    match v {
        Value::String(s) => Value::String(resolve_text(s, ids)),
        Value::Array(a) => Value::Array(a.iter().map(|x| resolve_value(x, ids)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), resolve_value(x, ids)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The id of the local name `name`, else the name itself (a local id of the same write).
fn id_of(name: &str, ids: &BTreeMap<String, String>) -> String {
    ids.get(name).cloned().unwrap_or_else(|| name.to_string())
}

/// A link map as `rdy_write` takes it.
fn link_input(links: &BTreeMap<String, Vec<String>>, ids: &BTreeMap<String, String>) -> Value {
    Value::Object(
        links
            .iter()
            .map(|(k, names)| {
                let targets: Vec<Value> = names
                    .iter()
                    .map(|n| json!({"target": id_of(n, ids)}))
                    .collect();
                (k.clone(), Value::Array(targets))
            })
            .collect(),
    )
}

/// Answers each exception card beside the proposal `pref` with `confirm`, then the approval card
/// with `approve`, relaying `relay`: the landing hash.
fn relay_approval(s: &mut Session, pref: &str, relay: &Relay) -> Result<(String, Value)> {
    let answer = |s: &mut Session, item: &Value, option: &str| -> Result<Value> {
        s.call(
            "rdy_answer",
            json!({"ref": item["ref"], "option": option, "card_hash": item["card_hash"],
                   "evidence": relay.evidence, "question": relay.question,
                   "session_ref": SESSION_REF}),
        )
    };
    let pending = s.call("rdy_pending", json!({}))?;
    for item in pending["items"].as_array().into_iter().flatten() {
        if item["batch"] == pref && item["kind"] != "approve" {
            answer(s, item, "confirm")?;
        }
    }
    let pending = s.call("rdy_pending", json!({}))?;
    let card = pending["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|i| i["ref"] == pref && i["kind"] == "approve")
        .cloned()
        .ok_or(s.stage.clone())?;
    let v = answer(s, &card, "approve")?;
    let landing = v["landing"]["landing"]
        .as_str()
        .filter(|l| is_hex(l, 64))
        .ok_or(s.stage.clone())?
        .to_string();
    Ok((landing, card))
}

/// Asks for the tag `t` on canon's head, relaying the person's words: the tag reply.
fn relay_tag(s: &mut Session, t: &TagSpec, landing: &str) -> Result<Value> {
    let v = s.call(
        "rdy_tag",
        json!({"version": t.version, "message": t.message, "evidence": t.evidence,
               "question": t.question, "session_ref": SESSION_REF}),
    )?;
    if v["landing"] != landing || v["name"] != t.version.as_str() || v["tagged_by"] != HUMAN_ACTOR {
        return Err(s.stage.clone());
    }
    Ok(v)
}

// ---------------------------------------------------------------- the frozen sample

/// The inputs of a check: the `rdy` under check, the frozen dump, its freeze record, the build
/// list, and a new work directory (its parent must exist).
#[derive(Clone, Debug)]
pub struct Sample {
    pub rdy: PathBuf,
    pub dump: PathBuf,
    pub freeze: PathBuf,
    pub build_list: PathBuf,
    pub work: PathBuf,
}

/// A frozen dump that verified and loaded.
pub struct Frozen {
    pub rdy: PathBuf,
    pub list: BuildList,
    pub freeze: Freeze,
    pub freeze_sha256: String,
    pub dump: Vec<u8>,
    /// The loaded project.
    pub project: PathBuf,
    /// The reply of `rdy load --json`.
    pub load: Value,
    /// The reply of `rdy fsck --json`.
    pub fsck: Value,
    person: Person,
}

/// Verifies the frozen dump against its record, copies the verified bytes into the new work
/// directory and loads that copy into `<work>/project`, then runs fsck. The load must report the
/// recorded formats, one landing and one tag.
pub fn load_frozen(s: &Sample) -> Result<Frozen> {
    let list = read_build_list(&s.build_list)?;
    let (freeze, freeze_sha256) = read_freeze(&s.freeze)?;
    let dump = read_bounded(&s.dump, DUMP_MAX)?;
    verify_frozen(&dump, &freeze, &list)?;
    std::fs::create_dir(&s.work).map_err(|_| SampleFailure::File)?;
    let person = Person::make(&s.work)?;
    let copy = s.work.join(DUMP_NAME);
    write_new(&copy, &dump)?;
    let project = s.work.join("project");
    let load = run_json(
        &s.rdy,
        &person,
        &[
            "--json".as_ref(),
            "load".as_ref(),
            "--no-daemon".as_ref(),
            copy.as_os_str(),
            project.as_os_str(),
        ],
        SampleFailure::FrozenDump,
    )?;
    if load["dump_format"] != freeze.dump_format
        || load["store_format"] != freeze.store_format
        || load["landings"] != 1
        || load["tags"] != 1
    {
        return Err(SampleFailure::FrozenDump);
    }
    let fsck = run_json(
        &s.rdy,
        &person,
        &[
            "--json".as_ref(),
            "fsck".as_ref(),
            "--no-daemon".as_ref(),
            "--project".as_ref(),
            project.as_os_str(),
        ],
        SampleFailure::FrozenDump,
    )?;
    Ok(Frozen {
        rdy: s.rdy.clone(),
        list,
        freeze,
        freeze_sha256,
        dump,
        project,
        load,
        fsck,
        person,
    })
}

/// Whether `b` may continue an actor name.
fn name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/')
}

/// The places and names of this machine that must not stand in the dump: each that is at least
/// 3 bytes and is not text of the build list.
fn machine_texts(allowed: &str) -> Vec<String> {
    let mut out = Vec::new();
    for var in [
        "USER",
        "USERNAME",
        "LOGNAME",
        "HOSTNAME",
        "COMPUTERNAME",
        "HOME",
        "USERPROFILE",
        "TMPDIR",
    ] {
        if let Ok(v) = std::env::var(var) {
            out.push(v);
        }
    }
    out.push(std::env::temp_dir().to_string_lossy().into_owned());
    if let Ok(d) = std::env::current_dir() {
        out.push(d.to_string_lossy().into_owned());
    }
    out.retain(|v| v.len() >= 3 && !allowed.contains(v.as_str()));
    out
}

/// The audit of the whole history in the dump bytes: every recorded actor is one of the two
/// sample actors, and no place, host, address, key or session state of a machine stands in it.
/// `allowed` is the build list's text: the approved content.
pub fn audit_history(dump: &[u8], allowed: &str) -> Result<()> {
    let fail = Err(SampleFailure::BaselineAudit);
    let at = |needle: &[u8]| -> Vec<usize> {
        dump.windows(needle.len())
            .enumerate()
            .filter(|(_, w)| *w == needle)
            .map(|(i, _)| i)
            .collect()
    };
    if !at(b"ci:").is_empty() {
        return fail;
    }
    for (prefix, name) in [("human:", "sample-author"), ("agent:", "sample-agent")] {
        let names = at(format!("{prefix}{name}").as_bytes());
        if names.is_empty() {
            return fail;
        }
        for i in at(prefix.as_bytes()) {
            let end = i + prefix.len() + name.len();
            let before = i.checked_sub(1).map(|j| dump[j]);
            if !names.contains(&i)
                || dump.get(end).is_some_and(|b| name_byte(*b))
                || before.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'/')
            {
                return fail;
            }
        }
    }
    for f in FORBIDDEN
        .iter()
        .map(|s| s.to_string())
        .chain(machine_texts(allowed))
    {
        if !at(f.as_bytes()).is_empty() {
            return fail;
        }
    }
    static EMAIL: OnceLock<regex::bytes::Regex> = OnceLock::new();
    let email = EMAIL.get_or_init(|| {
        regex::bytes::Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")
            .expect("the address pattern")
    });
    if email.is_match(dump) {
        return fail;
    }
    Ok(())
}

/// Every string in `v` that names an actor is a sample actor, and no string holds forbidden text.
fn clean_strings(v: &Value) -> bool {
    let mut all = Vec::new();
    strings_in(v, &mut all);
    all.iter().all(|s| {
        let actor = ["human:", "agent:", "ci:"].iter().any(|p| s.starts_with(p));
        (!actor || s == HUMAN_ACTOR || s == AGENT_ACTOR) && !FORBIDDEN.iter().any(|f| s.contains(f))
    })
}

/// What the audit of the loaded baseline read.
pub struct Baseline {
    pub log: Value,
    pub views: Value,
    pub tags: Value,
    pub pending: Value,
    pub publication: Value,
}

/// The audit of the loaded baseline: the project config, one landing with its tag, the actors of
/// every recorded landing, view and tag, no open view or pending card, every recorded id, and
/// the docs and blocks of the build list.
pub fn audit_baseline(f: &Frozen) -> Result<Baseline> {
    let fail = SampleFailure::BaselineAudit;
    let config = read_bounded(&f.project.join(".rdy.yaml"), RECORD_MAX)?;
    if config != render_config(&f.list)?.as_bytes() {
        return Err(fail);
    }
    let mut s = Session::start(
        &f.rdy,
        &f.person,
        &f.project,
        Some(meta(AUDIT_RUN)),
        fail.clone(),
    )?;
    s.instructions()?;
    let log = s.call("rdy_log", json!({}))?;
    let views = s.call("rdy_view", json!({"op": "list", "all": true}))?;
    let tags = s.call("rdy_tags", json!({}))?;
    let pending = s.call("rdy_pending", json!({}))?;
    let publication = s.call("rdy_publication_check", json!({"profile": "implicit:org"}))?;
    for id in f.freeze.ids.values() {
        s.call("rdy_read", json!({"id": id, "scope": "canon"}))?;
    }
    s.finish()?;
    let landings = log.as_array().cloned().unwrap_or_default();
    let versions = tags["versions"].as_array().cloned().unwrap_or_default();
    let ok = landings.len() == 1
        && landings[0]["landing"] == f.freeze.landing.as_str()
        && landings[0]["tags"] == json!([f.freeze.tag])
        && landings[0]["approved_by"] == HUMAN_ACTOR
        && landings[0]["proposed_by"]["actor"] == AGENT_ACTOR
        && landings[0]["relayed_by"]["actor"] == AGENT_ACTOR
        && versions.len() == 1
        && versions[0]["name"] == f.freeze.tag.as_str()
        && versions[0]["landing"] == f.freeze.landing.as_str()
        && versions[0]["tagged_by"] == HUMAN_ACTOR
        && views["views"].as_array().is_some_and(|v| {
            v.iter()
                .all(|x| x["closed"] == true && x["owner"]["actor"] == AGENT_ACTOR)
        })
        && pending["items"] == json!([])
        && pending["not_ready"] == json!([])
        && publication["selection"]["docs"] == f.list.counts().1
        && publication["selection"]["blocks"] == f.list.total_blocks()
        && [&log, &views, &tags, &pending]
            .iter()
            .all(|v| clean_strings(v));
    if !ok {
        return Err(fail);
    }
    Ok(Baseline {
        log,
        views,
        tags,
        pending,
        publication,
    })
}

/// What the first session returned.
pub struct FirstSession {
    /// The briefing of the old decision and its section, before the supersede.
    pub brief_before: Value,
    pub supersede: Value,
    /// The new decision.
    pub d7: String,
    pub fix: Value,
    /// The briefing of the old decision after the fix.
    pub brief_after: Value,
    /// The approval card the person answered.
    pub card: Value,
    pub landing: String,
    pub tag: Value,
    pub transcript_sha256: String,
    pub calls: u64,
}

/// Whether the block `name` of the build list states `value` in its body or fields.
fn states(list: &BuildList, name: &str, value: &str) -> bool {
    all_blocks(&list.folders)
        .into_iter()
        .filter(|b| b.name.as_deref() == Some(name))
        .any(|b| {
            let mut all = Vec::new();
            strings_in(&Value::Object(b.fields.clone()), &mut all);
            all.extend(b.body.clone());
            all.iter().any(|s| s.contains(value))
        })
}

/// The first session on the loaded copy, through the real stdio MCP path: session and
/// instructions, the search and the briefing, the supersede, the fix write, the checks, the plan
/// answers, the proposal, the person's relayed approval and the tag. Each step's reply must hold
/// what the published sequence says (D-Q69VB): a dependent that states a removed value gets
/// `update`, with the value matched in its text; one that states none gets `confirm`.
pub fn first_session(f: &Frozen) -> Result<FirstSession> {
    let fail = SampleFailure::FirstSession;
    let list = &f.list;
    let ids = &f.freeze.ids;
    let id = |n: &str| -> Result<String> { ids.get(n).cloned().ok_or(SampleFailure::FirstSession) };
    let old = id(&list.supersede_old)?;
    let section = id(&list
        .parent_of(&list.supersede_old)
        .ok_or(fail.clone())?
        .0
        .clone())?;
    let targets: Vec<String> = list
        .fix_ops
        .iter()
        .map(|o| o["id"].as_str().unwrap_or("").to_string())
        .collect();
    let target_ids: Vec<String> = targets.iter().map(|t| id(t)).collect::<Result<_>>()?;
    let mut s = Session::start(&f.rdy, &f.person, &f.project, None, fail.clone())?;
    s.call(
        "rdy_session",
        json!({"actor": AGENT_ACTOR, "run": SESSION_RUN}),
    )?;
    s.instructions()?;
    // step 4: the search finds the dependents, and the briefing names them
    let found = s.call("rdy_search", json!({"query": list.search}))?;
    let hits: BTreeSet<&str> = found["matches"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str())
        .collect();
    let brief_before = s.call("rdy_brief", json!({"ids": [old, section]}))?;
    for t in &target_ids {
        let depends = |kind: &str| {
            brief_before["dependents"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|d| d["block"] == t.as_str() && d["kind"] == kind && d["on"] == old.as_str())
        };
        let stale = brief_before["will_go_stale"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|w| w["block"] == t.as_str());
        if !hits.contains(t.as_str()) || !depends("decided_by") || !depends("implements") || !stale
        {
            return Err(fail);
        }
    }
    // step 5: the supersede, whose consequences are exactly the dependents
    let mut decision = resolve_value(&Value::Object(list.supersede_decision.clone()), ids);
    decision["parent"] = json!(section);
    decision["brief"] = brief_before["base"]["token"].clone();
    let supersede = s.call("rdy_supersede", json!({"old": old, "decision": decision}))?;
    let d7 = supersede["created"]["$decision"]
        .as_str()
        .ok_or(fail.clone())?
        .to_string();
    let consequences = supersede["consequences"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if consequences.len() != target_ids.len() {
        return Err(fail);
    }
    for (name, t) in targets.iter().zip(&target_ids) {
        let c = consequences
            .iter()
            .find(|c| c["block"] == t.as_str())
            .ok_or(fail.clone())?;
        let values: Vec<String> = c["because"]["values"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        let stated = values.iter().any(|v| states(list, name, v));
        let matched = c["matched"].as_array().cloned().unwrap_or_default();
        let ok = c["kind"] == "dependent"
            && c["because"]["changed"] == old.as_str()
            && c["because"]["edges"] == json!(["decided_by", "implements"])
            && !values.is_empty()
            && if stated {
                c["follow_up"] == "update"
                    && !matched.is_empty()
                    && matched
                        .iter()
                        .all(|m| values.contains(&m["value"].as_str().unwrap_or("").to_string()))
            } else {
                c["follow_up"] == "confirm" && c.get("matched").is_none()
            };
        if !ok {
            return Err(fail);
        }
    }
    // step 6: the fix write moves both links of each dependent to the new decision
    let mut ids7 = ids.clone();
    ids7.insert(list.supersede_name.clone(), d7.clone());
    let brief = s.call("rdy_brief", json!({"ids": target_ids}))?;
    let mut ops = Vec::new();
    for op in &list.fix_ops {
        let mut o = resolve_value(op, &ids7);
        o["id"] = json!(id_of(op["id"].as_str().unwrap_or(""), &ids7));
        if let Some(l) = op.get("links").and_then(Value::as_object) {
            let mut map = BTreeMap::new();
            for (k, names) in l {
                map.insert(k.clone(), texts(names).map_err(|_| fail.clone())?);
            }
            o["links"] = link_input(&map, &ids7);
        }
        ops.push(o);
    }
    let fix = s.call(
        "rdy_write",
        json!({"ops": ops, "implements": [d7], "brief": brief["base"]["token"]}),
    )?;
    if fix["consequences"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|c| c["because"]["changed"] == old.as_str())
    {
        return Err(fail);
    }
    // step 7: no suspect, no tension, nothing left on the old decision; the lineage stays
    let suspects = s.call("rdy_suspects", json!({}))?;
    let tensions = s.call("rdy_tensions", json!({}))?;
    let brief_after = s.call("rdy_brief", json!({"ids": [old]}))?;
    let lineage = brief_after["lineage"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|l| l["block"] == d7.as_str() && l["kind"] == "supersedes" && l["direction"] == "in");
    let left = brief_after["dependents"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|d| target_ids.iter().any(|t| d["block"] == t.as_str()));
    if suspects["items"] != json!([])
        || tensions["tensions"] != json!([])
        || brief_after["will_go_stale"] != json!([])
        || brief_after["suspects"] != json!([])
        || left
        || !lineage
    {
        return Err(fail);
    }
    // the change plan: only confirm items are open, and each still holds
    let plan = s.call("rdy_plan", json!({}))?;
    let open: Vec<&Value> = plan["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["state"] == "open")
        .collect();
    if open.iter().any(|i| i["kind"] != "confirm") {
        return Err(fail);
    }
    if !open.is_empty() {
        let after = s.call(
            "rdy_plan",
            json!({"answers": [{"act": "confirm", "group": {"kind": "confirm"},
                                 "reason": list.confirm_reason}]}),
        )?;
        if after["counts"]["open"] != 0 {
            return Err(fail);
        }
    }
    // steps 8 to 10: the proposal, the person's relayed approval and the tag
    let p = s.call(
        "rdy_propose",
        json!({"title": list.session_title, "summary": list.session_summary}),
    )?;
    let pref = p["ref"].as_str().ok_or(fail.clone())?.to_string();
    if p["ready"] != true || p["issues"] != json!([]) {
        return Err(fail);
    }
    let (landing, card) = relay_approval(&mut s, &pref, &list.session_approval)?;
    let fact = |name: &str| {
        card["facts"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|x| x["name"] == name)
            .map(|x| x["value"].clone())
    };
    if landing == f.freeze.landing
        || fact("Supersedes") != Some(json!(old))
        || fact("Tensions") != Some(json!("0 blocking, 0 advisory"))
    {
        return Err(fail);
    }
    let tag = relay_tag(&mut s, &list.session_tag, &landing)?;
    let (transcript_sha256, calls) = s.finish()?;
    Ok(FirstSession {
        brief_before,
        supersede,
        d7,
        fix,
        brief_after,
        card,
        landing,
        tag,
        transcript_sha256,
        calls,
    })
}

/// What the reopened project holds.
pub struct Reopened {
    pub tags: Value,
    pub publication: Value,
    /// The flow dependent as YAML.
    pub flow: String,
    /// The requirement dependent as YAML.
    pub requirement: String,
}

/// Reopens the project in a new MCP session and verifies the first session's landed result:
/// both tags, the superseded decision's lineage, the fixed dependents, no suspect or tension,
/// and the `implicit:org` publication check of the new landing.
pub fn reopen(f: &Frozen, first: &FirstSession) -> Result<Reopened> {
    let fail = SampleFailure::FirstSession;
    let list = &f.list;
    let ids = &f.freeze.ids;
    let old = ids.get(&list.supersede_old).ok_or(fail.clone())?.clone();
    let mut s = Session::start(&f.rdy, &f.person, &f.project, None, fail.clone())?;
    s.call(
        "rdy_session",
        json!({"actor": AGENT_ACTOR, "run": REOPEN_RUN}),
    )?;
    s.instructions()?;
    let tags = s.call("rdy_tags", json!({}))?;
    let log = s.call("rdy_log", json!({}))?;
    let brief = s.call("rdy_brief", json!({"ids": [old]}))?;
    let suspects = s.call("rdy_suspects", json!({}))?;
    let tensions = s.call("rdy_tensions", json!({}))?;
    let publication = s.call("rdy_publication_check", json!({"profile": "implicit:org"}))?;
    let mut read = |name: &str| -> Result<String> {
        let id = ids.get(name).ok_or(SampleFailure::FirstSession)?;
        let v = s.call(
            "rdy_read",
            json!({"id": id, "scope": "canon", "format": "yaml"}),
        )?;
        v["text"]
            .as_str()
            .map(str::to_string)
            .ok_or(SampleFailure::FirstSession)
    };
    let mut texts_of = BTreeMap::new();
    for op in &list.fix_ops {
        let name = op["id"].as_str().unwrap_or("").to_string();
        texts_of.insert(name.clone(), read(&name)?);
    }
    s.finish()?;
    let flow = texts_of
        .iter()
        .find(|(n, _)| list.type_of(n) == Some("flow"))
        .map(|(_, t)| t.clone())
        .unwrap_or_default();
    let requirement = texts_of
        .iter()
        .find(|(n, _)| list.type_of(n) == Some("requirement"))
        .map(|(_, t)| t.clone())
        .unwrap_or_default();
    let versions = tags["versions"].as_array().cloned().unwrap_or_default();
    let names: Vec<&str> = versions.iter().filter_map(|v| v["name"].as_str()).collect();
    let lineage = brief["lineage"].as_array().into_iter().flatten().any(|l| {
        l["block"] == first.d7.as_str() && l["kind"] == "supersedes" && l["direction"] == "in"
    });
    let fixed = texts_of
        .values()
        .all(|t| t.contains(&first.d7) && !t.contains(&old));
    let ok = names == [list.tag.version.as_str(), list.session_tag.version.as_str()]
        && versions[0]["landing"] == f.freeze.landing.as_str()
        && versions[1]["landing"] == first.landing.as_str()
        && log.as_array().map(Vec::len) == Some(2)
        && lineage
        && brief["will_go_stale"] == json!([])
        && brief["suspects"] == json!([])
        && suspects["items"] == json!([])
        && tensions["tensions"] == json!([])
        && publication["profile"]["audience"] == "org"
        && publication["references"]["status"] == "clear"
        && publication["scope"]["landing"] == first.landing.as_str()
        && fixed;
    if !ok {
        return Err(fail);
    }
    Ok(Reopened {
        tags,
        publication,
        flow,
        requirement,
    })
}

/// The receipt of a passing check, as one JSON object with the keys of [`RECEIPT_KEYS`].
pub type Receipt = Map<String, Value>;

/// The whole check: load the frozen dump, audit its history and its baseline, run the first
/// session, reopen and verify. The receipt, or the first failure.
pub fn check(s: &Sample) -> Result<Receipt> {
    let f = load_frozen(s)?;
    audit_history(&f.dump, &f.list.text)?;
    let b = audit_baseline(&f)?;
    let first = first_session(&f)?;
    reopen(&f, &first)?;
    let version = rdy_version(&f.rdy, &f.person, SampleFailure::FirstSession)?;
    let binary = read_bounded(&f.rdy, BINARY_MAX)?;
    let baseline_landing = b.log[0]["landing"].clone();
    let values = [
        json!(RECEIPT_FORMAT),
        json!("pass"),
        json!(version),
        json!(format!(
            "{}-{}",
            std::env::consts::ARCH,
            std::env::consts::OS
        )),
        json!(sha256_hex(&binary)),
        json!(DUMP_NAME),
        json!(f.freeze.size),
        json!(f.freeze.sha256),
        json!(f.freeze.blake3),
        json!(f.freeze_sha256),
        json!(f.list.sha256),
        baseline_landing,
        json!(f.freeze.tag),
        json!(first.landing),
        first.tag["name"].clone(),
        json!(first.transcript_sha256),
        json!(first.calls),
        json!(0),
    ];
    Ok(RECEIPT_KEYS
        .iter()
        .map(|k| k.to_string())
        .zip(values)
        .collect())
}

// ---------------------------------------------------------------- the builder

/// The inputs of the builder: the `rdy` that builds, the build list, a new work directory, the
/// new dump and freeze record files, and the 40-hex commit the builder runs from.
#[derive(Clone, Debug)]
pub struct BuildInput {
    pub rdy: PathBuf,
    pub build_list: PathBuf,
    pub work: PathBuf,
    pub out: PathBuf,
    pub freeze: PathBuf,
    pub builder_revision: String,
}

/// Whether a listed block names a decision of the list: by a link, an `affects` entry or a text
/// reference.
fn names_decision(b: &BlockSpec, decisions: &BTreeSet<String>) -> bool {
    let mut refs = Vec::new();
    let mut v = Map::new();
    v.insert("fields".into(), Value::Object(b.fields.clone()));
    v.insert("body".into(), json!(b.body));
    let _ = text_refs(&Value::Object(v), &mut refs);
    b.links.values().flatten().any(|n| decisions.contains(n))
        || refs.iter().any(|n| decisions.contains(n))
}

/// The create op of the listed block `b` under `parent`.
fn create_op(b: &BlockSpec, parent: &str, ids: &BTreeMap<String, String>) -> Value {
    let mut op = json!({"op": "create", "type": b.block_type, "parent": parent});
    if let Some(n) = &b.name {
        op["id"] = json!(n);
    }
    if !b.fields.is_empty() {
        op["fields"] = resolve_value(&Value::Object(b.fields.clone()), ids);
    }
    if let Some(body) = &b.body {
        op["body"] = json!(resolve_text(body, ids));
    }
    if !b.links.is_empty() {
        op["links"] = link_input(&b.links, ids);
    }
    op
}

/// Splits the blocks under `parent` into the first write (no decision named) and the later ones
/// (a decision, or a block that names one, with every block under it), in document order. A later
/// block keeps its place before its next first-write sibling.
fn plan_blocks<'a>(
    blocks: &'a [BlockSpec],
    parent: &str,
    decisions: &BTreeSet<String>,
    first: &mut Vec<(&'a BlockSpec, String)>,
    later: &mut Vec<(&'a BlockSpec, String, Option<usize>)>,
) {
    for (i, b) in blocks.iter().enumerate() {
        if b.block_type == "decision" || names_decision(b, decisions) {
            // the next sibling that the first write creates, by its index in `blocks`
            let next = blocks[i + 1..]
                .iter()
                .position(|x| x.block_type != "decision" && !names_decision(x, decisions))
                .map(|p| i + 1 + p);
            later.push((b, parent.to_string(), next));
            continue;
        }
        first.push((b, parent.to_string()));
        if let Some(n) = &b.name {
            plan_blocks(&b.blocks, n, decisions, first, later);
        }
    }
}

/// Builds the baseline of the build list with `b.rdy` in a new project, lands and tags it, dumps
/// it to `b.out`, loads the dump into a new directory and audits it, then writes `b.freeze`. It
/// never runs the first session in the project that supplies the dump. The freeze record.
pub fn build(b: &BuildInput) -> Result<Freeze> {
    let fail = SampleFailure::BaselineAudit;
    let list = read_build_list(&b.build_list)?;
    if !is_hex(&b.builder_revision, 40) {
        return Err(SampleFailure::InvalidArguments);
    }
    std::fs::create_dir(&b.work).map_err(|_| SampleFailure::File)?;
    let person = Person::make(&b.work)?;
    let project = b.work.join("project");
    run_json(
        &b.rdy,
        &person,
        &[
            "--json".as_ref(),
            "init".as_ref(),
            "--no-daemon".as_ref(),
            "--pack".as_ref(),
            list.pack.as_ref(),
            project.as_os_str(),
        ],
        fail.clone(),
    )?;
    let config_file = project.join(".rdy.yaml");
    std::fs::write(&config_file, render_config(&list)?).map_err(|_| SampleFailure::File)?;
    let version = rdy_version(&b.rdy, &person, fail.clone())?;

    let mut s = Session::start(
        &b.rdy,
        &person,
        &project,
        Some(meta(&list.view)),
        fail.clone(),
    )?;
    s.instructions()?;
    // the first write: the folders, the docs, then every block that names no decision
    let mut ops = Vec::new();
    for f in &list.folders {
        ops.push(json!({"op": "create", "type": "folder", "id": f.name, "fields": f.fields}));
    }
    for f in &list.folders {
        for d in &f.docs {
            ops.push(
                json!({"op": "create", "type": "doc", "id": d.name, "parent": f.name,
                            "fields": d.fields}),
            );
        }
    }
    let mut first = Vec::new();
    let mut later = Vec::new();
    for d in list.folders.iter().flat_map(|f| &f.docs) {
        plan_blocks(&d.blocks, &d.name, &list.decisions, &mut first, &mut later);
    }
    let none = BTreeMap::new();
    for (blk, parent) in &first {
        ops.push(create_op(blk, parent, &none));
    }
    let w = s.call(
        "rdy_write",
        json!({"ops": ops, "message": list.write_message}),
    )?;
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    let created = |w: &Value, ids: &mut BTreeMap<String, String>| {
        for (k, v) in w["created"].as_object().into_iter().flatten() {
            if is_local(k)
                && let Some(id) = v.as_str()
            {
                ids.insert(k.clone(), id.to_string());
            }
        }
    };
    created(&w, &mut ids);
    // each decision under its briefed parent
    for (blk, parent, _) in later.iter().filter(|(b, _, _)| b.block_type == "decision") {
        let parent_id = id_of(parent, &ids);
        let brief = s.call("rdy_brief", json!({"ids": [parent_id]}))?;
        let mut args = resolve_value(
            &Value::Object(blk.decision.clone().unwrap_or_default()),
            &ids,
        );
        if let Some(a) = blk.decision.as_ref().and_then(|d| d.get("affects")) {
            let names = texts(a).map_err(|_| fail.clone())?;
            args["affects"] = json!(names.iter().map(|n| id_of(n, &ids)).collect::<Vec<_>>());
        }
        args["parent"] = json!(parent_id);
        args["brief"] = brief["base"]["token"].clone();
        let v = s.call("rdy_decide", args)?;
        let id = v["created"]["$decision"].as_str().ok_or(fail.clone())?;
        ids.insert(blk.name.clone().unwrap_or_default(), id.to_string());
    }
    // the blocks that name a decision, with the baseline's justification
    let content: Vec<_> = later
        .iter()
        .filter(|(b, _, _)| b.block_type != "decision")
        .collect();
    if !content.is_empty() {
        let parents: BTreeSet<String> = content.iter().map(|(_, p, _)| id_of(p, &ids)).collect();
        let brief = s.call("rdy_brief", json!({"ids": parents}))?;
        let mut ops = Vec::new();
        for (blk, parent, next) in &content {
            let mut op = create_op(blk, &id_of(parent, &ids), &ids);
            if let Some(i) = next {
                let siblings = siblings_of(&list, parent);
                if let Some(n) = siblings.get(*i).and_then(|x| x.name.as_ref()) {
                    op["before"] = json!(id_of(n, &ids));
                }
            }
            ops.push(op);
            if let Some(n) = &blk.name {
                let mut sub_first = Vec::new();
                let mut sub_later = Vec::new();
                plan_blocks(
                    &blk.blocks,
                    n,
                    &BTreeSet::new(),
                    &mut sub_first,
                    &mut sub_later,
                );
                for (c, p) in sub_first {
                    ops.push(create_op(c, &p, &ids));
                }
            }
        }
        let w = s.call(
            "rdy_write",
            json!({"ops": ops, "message": list.write_message,
                   "implements": [id_of(&list.write_implements, &ids)],
                   "brief": brief["base"]["token"]}),
        )?;
        created(&w, &mut ids);
    }
    // the view's checks, the plan, the proposal, the approval and the tag
    let suspects = s.call("rdy_suspects", json!({}))?;
    let tensions = s.call("rdy_tensions", json!({}))?;
    let plan = s.call("rdy_plan", json!({}))?;
    if suspects["items"] != json!([])
        || tensions["tensions"] != json!([])
        || plan["counts"]["open"] != 0
    {
        return Err(fail);
    }
    let p = s.call(
        "rdy_propose",
        json!({"title": list.proposal_title, "summary": list.proposal_summary}),
    )?;
    let pref = p["ref"].as_str().ok_or(fail.clone())?.to_string();
    if p["ready"] != true {
        return Err(fail);
    }
    let (landing, _) = relay_approval(&mut s, &pref, &list.approval)?;
    relay_tag(&mut s, &list.tag, &landing)?;
    s.call("rdy_view", json!({"op": "close", "name": list.view}))?;
    s.finish()?;

    // freeze one full dump
    let staged = b.work.join(DUMP_NAME);
    let dumped = run_json(
        &b.rdy,
        &person,
        &[
            "--json".as_ref(),
            "dump".as_ref(),
            "--no-daemon".as_ref(),
            "--project".as_ref(),
            project.as_os_str(),
            "--out".as_ref(),
            staged.as_os_str(),
        ],
        fail.clone(),
    )?;
    let bytes = read_bounded(&staged, DUMP_MAX)?;
    let dump_format = dumped["dump_format"].as_u64().ok_or(fail.clone())?;
    let mut recorded = BTreeMap::new();
    for n in &list.baseline_names {
        recorded.insert(n.clone(), ids.get(n).cloned().ok_or(fail.clone())?);
    }
    let freeze = Freeze {
        name: DUMP_NAME.into(),
        size: bytes.len() as u64,
        sha256: sha256_hex(&bytes),
        blake3: blake3_hex(&bytes),
        build_list_sha256: list.sha256.clone(),
        builder_revision: b.builder_revision.clone(),
        rdy_version: version,
        dump_format,
        store_format: dumped["store_format"].clone(),
        landing,
        tag: list.tag.version.clone(),
        ids: recorded,
    };
    let mut record = serde_json::to_string_pretty(&freeze).map_err(|_| SampleFailure::File)?;
    record.push('\n');
    if record.len() as u64 > RECORD_MAX {
        return Err(SampleFailure::SizeLimit);
    }
    let staged_freeze = b.work.join("freeze.json");
    write_new(&staged_freeze, record.as_bytes())?;
    // the audit of the frozen bytes, loaded into a new directory
    let f = load_frozen(&Sample {
        rdy: b.rdy.clone(),
        dump: staged.clone(),
        freeze: staged_freeze,
        build_list: b.build_list.clone(),
        work: b.work.join("verify"),
    })?;
    audit_history(&f.dump, &f.list.text)?;
    audit_baseline(&f)?;
    write_new(&b.out, &bytes)?;
    write_new(&b.freeze, record.as_bytes())?;
    Ok(freeze)
}

/// The listed blocks directly under the local name `parent` (a doc or a section).
fn siblings_of<'a>(list: &'a BuildList, parent: &str) -> &'a [BlockSpec] {
    fn find<'a>(blocks: &'a [BlockSpec], parent: &str) -> Option<&'a [BlockSpec]> {
        for b in blocks {
            if b.name.as_deref() == Some(parent) {
                return Some(&b.blocks);
            }
            if let Some(found) = find(&b.blocks, parent) {
                return Some(found);
            }
        }
        None
    }
    for d in list.folders.iter().flat_map(|f| &f.docs) {
        if d.name == parent {
            return &d.blocks;
        }
        if let Some(found) = find(&d.blocks, parent) {
            return found;
        }
    }
    &[]
}
