//! The check and the code generation of the compiled agent guide. `build.rs` and
//! `tests/embedded_guide.rs` both include this file, so every refusal a test sees is the build's.
//!
//! The input is the public export bundle `release/agent-guide.json` and its digest. The layout is
//! closed: a changed one needs a plan change here, never a more permissive parser.

use rdy_model::did_you_mean;
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;

pub const BUNDLE_PATH: &str = "release/agent-guide.json";
pub const SIDECAR_PATH: &str = "release/agent-guide.sha256";

pub const BUNDLE_MAX: usize = 1_048_576;
pub const SIDECAR_BYTES: usize = 65;
pub const DEPTH_MAX: usize = 8;
pub const RULESETS_MAX: usize = 12;
pub const ROWS_MAX: usize = 14;
pub const NAME_MAX: usize = 64;
pub const TITLE_MAX: usize = 256;
pub const WHEN_MAX: usize = 1_024;
pub const ROW_ID_MAX: usize = 96;
pub const ROW_TEXT_MAX: usize = 262_144;

const FIX: &str = "re-export the guide and its digest from one frozen landing";

/// The rulesets, in delivery order. The bootstrap alone is required.
pub const NAMES: [&str; RULESETS_MAX] = [
    "bootstrap",
    "write",
    "after-write",
    "approvals",
    "as-built",
    "read",
    "publish",
    "review",
    "diagrams",
    "parallel",
    "project-ops",
    "troubleshoot",
];

/// The bootstrap's slices, in order. Their texts rebuild its exported body.
const BOOTSTRAP_ROWS: [&str; 3] = [
    "rdy:bootstrap:before",
    "rdy:bootstrap:approval",
    "rdy:bootstrap:after",
];
const LOCKED_ROW: usize = 1;

/// The reviewed paragraph on relaying the person's exact words: the only locked row.
pub const APPROVAL_TEXT: &str =
    "Never approve, tag or answer on your own judgment. Relay the person's exact words.";

/// The ending of an edition whose bundle came from the development profile, marks included.
const DEVELOPMENT: &str = "-dev";

/// A refused input. `Display` gives the whole diagnostic with its fix.
#[derive(Debug, PartialEq, Eq)]
pub struct Refusal(String);

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "embedded guide: {}; {FIX}", self.0)
    }
}

fn invalid(field: impl fmt::Display, reason: impl fmt::Display) -> Refusal {
    Refusal(format!("invalid {field}: {reason}"))
}

fn exceeds(field: impl fmt::Display, limit: impl fmt::Display) -> Refusal {
    Refusal(format!("{field} exceeds {limit}"))
}

fn bytes_limit(limit: usize) -> String {
    format!("{limit} bytes")
}

pub struct Row {
    pub id: String,
    pub text: String,
    pub locked: bool,
}

pub struct Ruleset {
    pub name: String,
    pub title: String,
    pub when: String,
    pub required: bool,
    pub rows: Vec<Row>,
}

/// A checked bundle, with the exact bytes the digest covers.
pub struct Catalogue {
    pub edition: String,
    pub design_landing: String,
    pub sha256: String,
    pub rulesets: Vec<Ruleset>,
    pub bytes: Vec<u8>,
}

/// Reads and checks the two inputs under the workspace `root`.
pub fn load(root: &Path) -> Result<Catalogue, Refusal> {
    let bundle = read_bounded(root, BUNDLE_PATH, BUNDLE_MAX)?;
    let sidecar = read_bounded(root, SIDECAR_PATH, SIDECAR_BYTES)?;
    validate(&bundle, &sidecar)
}

/// The bytes of `rel`, refused by their size before a buffer for them exists.
fn read_bounded(root: &Path, rel: &str, limit: usize) -> Result<Vec<u8>, Refusal> {
    let file = std::fs::File::open(root.join(rel)).map_err(|e| read_failure(rel, &e))?;
    let len = file.metadata().map_err(|e| read_failure(rel, &e))?.len();
    if len > limit as u64 {
        return Err(exceeds(rel, bytes_limit(limit)));
    }
    let mut bytes = Vec::with_capacity(len as usize);
    // The file may grow after the size check: one byte over the limit is enough to refuse.
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| read_failure(rel, &e))?;
    if bytes.len() > limit {
        return Err(exceeds(rel, bytes_limit(limit)));
    }
    Ok(bytes)
}

fn read_failure(rel: &str, e: &std::io::Error) -> Refusal {
    if e.kind() == std::io::ErrorKind::NotFound {
        Refusal(format!("missing {rel}"))
    } else {
        Refusal(format!("missing {rel}: {e}"))
    }
}

/// Checks the bundle's size, then its digest, then its closed shape, in that order: nothing is
/// parsed before the digest holds.
pub fn validate(bundle: &[u8], sidecar: &[u8]) -> Result<Catalogue, Refusal> {
    if bundle.len() > BUNDLE_MAX {
        return Err(exceeds(BUNDLE_PATH, bytes_limit(BUNDLE_MAX)));
    }
    if sidecar.len() > SIDECAR_BYTES {
        return Err(exceeds(SIDECAR_PATH, bytes_limit(SIDECAR_BYTES)));
    }
    let named = match sidecar.split_last() {
        Some((b'\n', hex)) if hex.len() == 64 && hex.iter().all(is_lower_hex) => hex,
        _ => return Err(invalid(SIDECAR_PATH, "invalid provenance")),
    };
    let sha256: String =
        Sha256::digest(bundle)
            .iter()
            .fold(String::with_capacity(64), |mut out, byte| {
                let _ = write!(out, "{byte:02x}");
                out
            });
    if sha256.as_bytes() != named {
        return Err(Refusal(format!(
            "{BUNDLE_PATH} does not match {SIDECAR_PATH}"
        )));
    }
    let text = std::str::from_utf8(bundle).map_err(|_| invalid(BUNDLE_PATH, "invalid UTF-8"))?;
    let root = parse(text)?;
    let mut catalogue = shape(root)?;
    catalogue.sha256 = sha256;
    catalogue.bytes = bundle.to_vec();
    Ok(catalogue)
}

fn is_lower_hex(b: &u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(b)
}

/// One JSON value. Only the integer form of a number matters: `schema` is the only number.
enum Node {
    Null,
    Bool(bool),
    Int(u64),
    Number,
    Text(String),
    List(Vec<Node>),
    Map(BTreeMap<String, Node>),
}

/// Why the parser stopped, kept apart from serde_json's own syntax errors.
#[derive(Clone, Copy)]
enum Stop {
    Depth,
    DuplicateKey,
}

fn parse(text: &str) -> Result<Node, Refusal> {
    let stop = Cell::new(None);
    let mut de = serde_json::Deserializer::from_str(text);
    let parsed = NodeSeed {
        depth: 0,
        stop: &stop,
    }
    .deserialize(&mut de)
    .and_then(|node| de.end().map(|()| node));
    parsed.map_err(|_| match stop.get() {
        Some(Stop::Depth) => exceeds("depth", DEPTH_MAX),
        Some(Stop::DuplicateKey) => invalid(BUNDLE_PATH, "duplicate key"),
        None => invalid(BUNDLE_PATH, "invalid JSON"),
    })
}

#[derive(Clone, Copy)]
struct NodeSeed<'a> {
    depth: usize,
    stop: &'a Cell<Option<Stop>>,
}

impl NodeSeed<'_> {
    fn halt<E: de::Error>(&self, why: Stop) -> E {
        self.stop.set(Some(why));
        E::custom("stopped")
    }

    fn deeper<E: de::Error>(&self) -> Result<Self, E> {
        if self.depth >= DEPTH_MAX {
            return Err(self.halt(Stop::Depth));
        }
        Ok(NodeSeed {
            depth: self.depth + 1,
            stop: self.stop,
        })
    }
}

impl<'de> DeserializeSeed<'de> for NodeSeed<'_> {
    type Value = Node;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Node, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for NodeSeed<'_> {
    type Value = Node;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_unit<E>(self) -> Result<Node, E> {
        Ok(Node::Null)
    }

    fn visit_bool<E>(self, v: bool) -> Result<Node, E> {
        Ok(Node::Bool(v))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Node, E> {
        Ok(Node::Int(v))
    }

    fn visit_i64<E>(self, _: i64) -> Result<Node, E> {
        Ok(Node::Number)
    }

    fn visit_f64<E>(self, _: f64) -> Result<Node, E> {
        Ok(Node::Number)
    }

    fn visit_str<E>(self, v: &str) -> Result<Node, E> {
        Ok(Node::Text(v.to_string()))
    }

    fn visit_string<E>(self, v: String) -> Result<Node, E> {
        Ok(Node::Text(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
        let inner = self.deeper()?;
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(inner)? {
            items.push(item);
        }
        Ok(Node::List(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
        let inner = self.deeper()?;
        let mut items = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            if items.contains_key(&key) {
                return Err(self.halt(Stop::DuplicateKey));
            }
            let value = map.next_value_seed(inner)?;
            items.insert(key, value);
        }
        Ok(Node::Map(items))
    }
}

/// The value of an object whose keys are exactly `keys`.
fn closed(field: &str, node: Node, keys: &[&str]) -> Result<BTreeMap<String, Node>, Refusal> {
    let Node::Map(map) = node else {
        return Err(invalid(field, "wrong type"));
    };
    // In key order, so one input gives one diagnostic.
    if let Some(key) = map.keys().find(|k| !keys.contains(&k.as_str())) {
        let reason = match did_you_mean::nearest(key, keys) {
            Some(near) => format!("unknown key {key}; did you mean {near}?"),
            None => format!("unknown key {key}"),
        };
        return Err(invalid(field, reason));
    }
    Ok(map)
}

fn text_of(field: &str, node: Option<Node>) -> Result<String, Refusal> {
    match node {
        Some(Node::Text(s)) => Ok(s),
        _ => Err(invalid(field, "wrong type")),
    }
}

fn bool_of(field: &str, node: Option<Node>) -> Result<bool, Refusal> {
    match node {
        Some(Node::Bool(b)) => Ok(b),
        _ => Err(invalid(field, "wrong type")),
    }
}

fn list_of(field: &str, node: Option<Node>) -> Result<Vec<Node>, Refusal> {
    match node {
        Some(Node::List(v)) => Ok(v),
        _ => Err(invalid(field, "wrong type")),
    }
}

fn bounded_text(field: &str, node: Option<Node>, limit: usize) -> Result<String, Refusal> {
    let text = text_of(field, node)?;
    if text.len() > limit {
        return Err(exceeds(field, bytes_limit(limit)));
    }
    if text.trim().is_empty() {
        return Err(invalid(field, "empty text"));
    }
    Ok(text)
}

fn shape(root: Node) -> Result<Catalogue, Refusal> {
    let mut map = closed(
        "bundle",
        root,
        &["schema", "edition", "design_landing", "rulesets"],
    )?;
    if !matches!(map.remove("schema"), Some(Node::Int(1))) {
        return Err(invalid("schema", "unsupported schema"));
    }
    let edition = text_of("edition", map.remove("edition"))?;
    if !valid_edition(&edition) {
        return Err(invalid("edition", "invalid provenance"));
    }
    let design_landing = text_of("design_landing", map.remove("design_landing"))?;
    if design_landing.len() != 64 || !design_landing.as_bytes().iter().all(is_lower_hex) {
        return Err(invalid("design_landing", "invalid provenance"));
    }
    let nodes = list_of("rulesets", map.remove("rulesets"))?;
    if nodes.len() > RULESETS_MAX {
        return Err(exceeds("rulesets", RULESETS_MAX));
    }
    let total: usize = nodes.iter().map(row_count).sum();
    if total > ROWS_MAX {
        return Err(exceeds("rows", ROWS_MAX));
    }
    let mut seen_sets = BTreeSet::new();
    let mut seen_rows = BTreeSet::new();
    let mut rulesets = Vec::with_capacity(RULESETS_MAX);
    for (i, node) in nodes.into_iter().enumerate() {
        rulesets.push(ruleset(i, node, &mut seen_sets, &mut seen_rows)?);
    }
    if rulesets.len() < RULESETS_MAX {
        return Err(invalid("rulesets", "unexpected order"));
    }
    if !edition.ends_with(DEVELOPMENT) {
        for (i, set) in rulesets.iter().enumerate() {
            refuse_marks(i, set)?;
        }
    }
    Ok(Catalogue {
        edition,
        design_landing,
        sha256: String::new(),
        rulesets,
        bytes: Vec::new(),
    })
}

/// The rows `node` holds, when it has the shape to hold any.
fn row_count(node: &Node) -> usize {
    match node {
        Node::Map(map) => match map.get("rows") {
            Some(Node::List(rows)) => rows.len(),
            _ => 0,
        },
        _ => 0,
    }
}

/// `<major>.<minor>.<patch>` in digits, then the development ending if the bundle has one.
fn valid_edition(edition: &str) -> bool {
    let release = edition.strip_suffix(DEVELOPMENT).unwrap_or(edition);
    let parts: Vec<&str> = release.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn ruleset(
    i: usize,
    node: Node,
    seen_sets: &mut BTreeSet<String>,
    seen_rows: &mut BTreeSet<String>,
) -> Result<Ruleset, Refusal> {
    let field = format!("rulesets[{i}]");
    let mut map = closed(&field, node, &["name", "title", "when", "required", "rows"])?;
    let name_field = format!("{field}.name");
    let name = text_of(&name_field, map.remove("name"))?;
    if name.len() > NAME_MAX {
        return Err(exceeds(&name_field, bytes_limit(NAME_MAX)));
    }
    if !valid_name(&name) {
        return Err(invalid(&name_field, "invalid name"));
    }
    if !seen_sets.insert(name.clone()) {
        return Err(invalid(&name_field, "duplicate ruleset"));
    }
    if NAMES[i] != name {
        return Err(invalid(&name_field, "unexpected order"));
    }
    let title = bounded_text(&format!("{field}.title"), map.remove("title"), TITLE_MAX)?;
    let when = bounded_text(&format!("{field}.when"), map.remove("when"), WHEN_MAX)?;
    let required_field = format!("{field}.required");
    let required = bool_of(&required_field, map.remove("required"))?;
    let is_bootstrap = i == 0;
    if required != is_bootstrap {
        return Err(invalid(&required_field, "invalid required set"));
    }
    let rows_field = format!("{field}.rows");
    let nodes = list_of(&rows_field, map.remove("rows"))?;
    if nodes.is_empty() {
        return Err(invalid(&rows_field, "unexpected order"));
    }
    let mut rows = Vec::with_capacity(nodes.len().min(ROWS_MAX));
    for (j, node) in nodes.into_iter().enumerate() {
        rows.push(row(&name, &rows_field, j, node, seen_rows)?);
    }
    let expected = if is_bootstrap {
        BOOTSTRAP_ROWS.len()
    } else {
        1
    };
    if rows.len() != expected {
        return Err(invalid(&rows_field, "unexpected order"));
    }
    Ok(Ruleset {
        name,
        title,
        when,
        required,
        rows,
    })
}

fn row(
    name: &str,
    rows_field: &str,
    j: usize,
    node: Node,
    seen_rows: &mut BTreeSet<String>,
) -> Result<Row, Refusal> {
    let field = format!("{rows_field}[{j}]");
    let mut map = closed(&field, node, &["id", "text", "locked"])?;
    let id_field = format!("{field}.id");
    let id = text_of(&id_field, map.remove("id"))?;
    if id.len() > ROW_ID_MAX {
        return Err(exceeds(&id_field, bytes_limit(ROW_ID_MAX)));
    }
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(invalid(&id_field, "invalid id"));
    }
    if !seen_rows.insert(id.clone()) {
        return Err(invalid(&id_field, "duplicate row"));
    }
    let is_bootstrap = name == NAMES[0];
    let expected = if is_bootstrap {
        BOOTSTRAP_ROWS.get(j).map(|s| (*s).to_string())
    } else if j == 0 {
        Some(format!("rdy:{name}:body"))
    } else {
        None
    };
    if expected.as_deref() != Some(id.as_str()) {
        return Err(invalid(&id_field, "unexpected order"));
    }
    let text_field = format!("{field}.text");
    let text = bounded_text(&text_field, map.remove("text"), ROW_TEXT_MAX)?;
    let locked_field = format!("{field}.locked");
    let locked = bool_of(&locked_field, map.remove("locked"))?;
    let is_approval = is_bootstrap && j == LOCKED_ROW;
    if locked != is_approval {
        return Err(invalid(&locked_field, "invalid approval lock"));
    }
    if is_approval && text != APPROVAL_TEXT {
        return Err(invalid(&text_field, "invalid approval lock"));
    }
    Ok(Row { id, text, locked })
}

/// A release bundle holds no as-built mark: a mark paragraph, a marked note, or a marked lede, or
/// the mark of the bootstrap's approval paragraph, which ends the slice before it.
fn refuse_marks(i: usize, set: &Ruleset) -> Result<(), Refusal> {
    for (j, row) in set.rows.iter().enumerate() {
        let lede = if j == 0 {
            row.text.split_once("\n\n")
        } else {
            None
        };
        let marked_lede = lede.is_some_and(|(_, rest)| rest.starts_with("Planned "));
        if marked_lede
            || row.text.ends_with("\n\nPlanned ")
            || row
                .text
                .split('\n')
                .any(|line| line == "**Planned**" || line == "> **Planned**")
        {
            let field = format!("rulesets[{i}].rows[{j}].text");
            return Err(invalid(field, "invalid provenance"));
        }
    }
    Ok(())
}

/// The Rust source of the catalogue. The debug form of a string is a valid, exact literal.
pub fn generate(catalogue: &Catalogue) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "pub const EDITION: &str = {:?};", catalogue.edition);
    let _ = writeln!(
        out,
        "pub const DESIGN_LANDING: &str = {:?};",
        catalogue.design_landing
    );
    let _ = writeln!(
        out,
        "pub const BUNDLE_SHA256: &str = {:?};",
        catalogue.sha256
    );
    let _ = writeln!(
        out,
        "pub const BUNDLE: &[u8] = include_bytes!(\"agent-guide.json\");"
    );
    let _ = writeln!(out, "pub static RULESETS: &[Ruleset] = &[");
    for set in &catalogue.rulesets {
        let _ = writeln!(
            out,
            "    Ruleset {{ name: {:?}, title: {:?}, when: {:?}, required: {:?}, rows: &[",
            set.name, set.title, set.when, set.required
        );
        for row in &set.rows {
            let _ = writeln!(
                out,
                "        Row {{ id: {:?}, text: {:?}, locked: {:?} }},",
                row.id, row.text, row.locked
            );
        }
        let _ = writeln!(out, "    ] }},");
    }
    let _ = writeln!(out, "];");
    out
}
