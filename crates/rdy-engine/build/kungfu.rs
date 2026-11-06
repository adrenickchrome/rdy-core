//! The pure rules of the kungfu values (D-Y18E1) and the check of their tool table. `build.rs`,
//! `src/kungfu.rs` and `tests/kungfu.rs` all include this file, so it reads no generated catalogue:
//! the build and the running binary apply one byte rule and one line.
//!
//! A value is `kf-` and the first eight digest digits of the SHA-256 of a ruleset's source rows,
//! joined with no separator. Nothing is trimmed or normalized, and the delivered line stays outside
//! the digest. The table's layout is closed; `scripts/release/guide.py` checks the same rules.

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;

pub const TABLE_PATH: &str = "crates/rdy-engine/kungfu-tools.json";
pub const TABLE_MAX: usize = 32_768;
pub const ROWS_MAX: usize = 256;
pub const TOOL_MAX: usize = 96;
/// The bytes of a value: `kf-` and eight digits.
pub const VALUE_BYTES: usize = 11;

/// The rulesets that own tools, in delivery order, with no `rdy:` prefix.
pub const OWNERS: [&str; 9] = [
    "bootstrap",
    "write",
    "after-write",
    "approvals",
    "read",
    "publish",
    "review",
    "parallel",
    "project-ops",
];

/// The tools no ruleset owns, in tool-name order. They get no check and no count.
pub const EXEMPT: [&str; 2] = ["rdy_instructions", "rdy_session"];

/// A refused table. `Display` gives the whole diagnostic. No diagnostic prints a value.
#[derive(Debug, PartialEq, Eq)]
pub struct Refusal(String);

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "kungfu catalogue: {}", self.0)
    }
}

fn invalid() -> Refusal {
    Refusal("invalid table".to_string())
}

/// What a missing or wrong value does to a call. This release checks that every row warns; a row
/// that refuses needs a decision, a plan and tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Response {
    Warn,
    Refuse,
}

/// One checked row. A `None` ruleset means the tool is exempt.
#[derive(Debug, PartialEq, Eq)]
pub struct TableRow {
    pub tool: String,
    pub ruleset: Option<String>,
    pub response: Response,
}

/// The value of the source rows `texts`, in their order.
pub fn value_of<'a>(texts: impl IntoIterator<Item = &'a str>) -> String {
    let mut digest = Sha256::new();
    for text in texts {
        digest.update(text.as_bytes());
    }
    let sum = digest.finalize();
    let mut value = String::with_capacity(VALUE_BYTES);
    value.push_str("kf-");
    for byte in &sum[..4] {
        let _ = write!(value, "{byte:02x}");
    }
    value
}

/// The line added to an owner's final row. It ends with LF, and its tools are in ascending order.
pub fn suffix_of(value: &str, tools: &[&str]) -> String {
    let mut sorted = tools.to_vec();
    sorted.sort_unstable();
    format!(
        "On every call to a tool in this ruleset, pass kungfu: \"{value}\". Tools: {}.\n",
        sorted.join(", ")
    )
}

/// The bytes `suffix` adds to `text`: one LF comes first only when `text` does not end with LF.
pub fn added_bytes(text: &str, suffix: &str) -> usize {
    suffix.len() + usize::from(!text.ends_with('\n'))
}

/// `text` as delivered: its source bytes, exactly, then `suffix`.
pub fn delivered(text: &str, suffix: &str) -> String {
    let mut out = String::with_capacity(text.len() + added_bytes(text, suffix));
    out.push_str(text);
    if !text.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(suffix);
    out
}

/// Refuses two owners with one value. `sets` holds each owner's name and source rows.
pub fn check_values(sets: &[(&str, Vec<&str>)]) -> Result<(), Refusal> {
    check_values_with(sets, &|texts| value_of(texts.iter().copied()))
}

/// [`check_values`] with the derivation given. Equal text collides too; nothing salts or lengthens
/// a value to get past this check, so a collision needs a source correction.
pub fn check_values_with(
    sets: &[(&str, Vec<&str>)],
    derive: &dyn Fn(&[&str]) -> String,
) -> Result<(), Refusal> {
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    for (name, texts) in sets {
        if let Some(first) = seen.insert(derive(texts), name) {
            return Err(Refusal(format!("value collision rdy:{first} rdy:{name}")));
        }
    }
    Ok(())
}

/// Reads and checks the table under the workspace `root`. Its size is checked before a buffer for
/// it exists. A file that cannot be read is an invalid table.
pub fn load(root: &Path) -> Result<Vec<TableRow>, Refusal> {
    let file = std::fs::File::open(root.join(TABLE_PATH)).map_err(|_| invalid())?;
    let len = file.metadata().map_err(|_| invalid())?.len();
    if len > TABLE_MAX as u64 {
        return Err(over_bytes());
    }
    let mut bytes = Vec::with_capacity(len as usize);
    // The file may grow after the size check: one byte over the limit is enough to refuse.
    file.take(TABLE_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    validate(&bytes)
}

fn over_bytes() -> Refusal {
    Refusal(format!("table exceeds {TABLE_MAX} bytes"))
}

/// One row as written. A JSON object with each key exactly once: a null ruleset is an exemption, a
/// missing one is malformed, and an array is not a row.
struct RawRow {
    tool: String,
    ruleset: Option<String>,
    response: String,
}

impl<'de> Deserialize<'de> for RawRow {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<RawRow, D::Error> {
        d.deserialize_map(RowVisitor)
    }
}

struct RowVisitor;

impl<'de> Visitor<'de> for RowVisitor {
    type Value = RawRow;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a table row")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawRow, A::Error> {
        let (mut tool, mut ruleset, mut response) = (None, None, None);
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "tool" if tool.is_none() => tool = Some(map.next_value()?),
                "ruleset" if ruleset.is_none() => ruleset = Some(map.next_value()?),
                "response" if response.is_none() => response = Some(map.next_value()?),
                _ => return Err(de::Error::custom("an unknown or repeated key")),
            }
        }
        match (tool, ruleset, response) {
            (Some(tool), Some(ruleset), Some(response)) => Ok(RawRow {
                tool,
                ruleset,
                response,
            }),
            _ => Err(de::Error::custom("a missing key")),
        }
    }
}

/// The top-level array. The row bound stops the parse before any further row is kept.
struct Rows<'a> {
    over: &'a Cell<bool>,
}

impl<'de> Visitor<'de> for Rows<'_> {
    type Value = Vec<RawRow>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an array of rows")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<RawRow>, A::Error> {
        let mut rows = Vec::new();
        while let Some(row) = seq.next_element::<RawRow>()? {
            if rows.len() == ROWS_MAX {
                self.over.set(true);
                return Err(de::Error::custom("too many rows"));
            }
            rows.push(row);
        }
        Ok(rows)
    }
}

fn parse(bytes: &[u8]) -> Result<Vec<RawRow>, Refusal> {
    let over = Cell::new(false);
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let parsed = de
        .deserialize_seq(Rows { over: &over })
        .and_then(|rows| de.end().map(|()| rows));
    parsed.map_err(|_| {
        if over.get() {
            Refusal(format!("table exceeds {ROWS_MAX} rows"))
        } else {
            invalid()
        }
    })
}

fn valid_tool(tool: &str) -> bool {
    !tool.is_empty()
        && tool.len() <= TOOL_MAX
        && tool
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Checks the table's bytes, its rows one by one in order, then the closed sets of owners and
/// exemptions. A malformed field is an invalid table.
pub fn validate(bytes: &[u8]) -> Result<Vec<TableRow>, Refusal> {
    if bytes.len() > TABLE_MAX {
        return Err(over_bytes());
    }
    let raw = parse(bytes)?;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut owners: BTreeSet<String> = BTreeSet::new();
    let mut exempt = 0;
    let mut rows: Vec<TableRow> = Vec::with_capacity(raw.len());
    for row in raw {
        if !valid_tool(&row.tool) {
            return Err(invalid());
        }
        if seen.contains(&row.tool) {
            return Err(Refusal(format!("duplicate tool {}", row.tool)));
        }
        if rows.last().is_some_and(|last| last.tool > row.tool) {
            return Err(invalid());
        }
        match &row.ruleset {
            Some(name) => {
                if !name.bytes().all(|b| b.is_ascii_graphic()) {
                    return Err(invalid());
                }
                let owner = name
                    .strip_prefix("rdy:")
                    .filter(|owner| OWNERS.contains(owner));
                let Some(owner) = owner else {
                    return Err(Refusal(format!("unknown ruleset {name}")));
                };
                owners.insert(owner.to_string());
            }
            None if EXEMPT.contains(&row.tool.as_str()) => exempt += 1,
            None => return Err(invalid()),
        }
        let response = match row.response.as_str() {
            "warn" => Response::Warn,
            "refuse" => Response::Refuse,
            _ => return Err(invalid()),
        };
        if response != Response::Warn {
            return Err(Refusal(format!("non-warn response {}", row.tool)));
        }
        seen.insert(row.tool.clone());
        rows.push(TableRow {
            tool: row.tool,
            ruleset: row.ruleset,
            response,
        });
    }
    if owners.len() != OWNERS.len() || exempt != EXEMPT.len() {
        return Err(invalid());
    }
    Ok(rows)
}

/// The Rust source of the static rows. The debug form of a string is a valid, exact literal.
pub fn generate(rows: &[TableRow]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "pub static TOOLS: &[Policy] = &[");
    for row in rows {
        let _ = writeln!(
            out,
            "    Policy {{ tool: {:?}, ruleset: {:?}, response: Response::{:?} }},",
            row.tool,
            row.ruleset.as_deref(),
            row.response
        );
    }
    let _ = writeln!(out, "];");
    out
}
