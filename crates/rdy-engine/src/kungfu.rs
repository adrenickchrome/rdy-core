//! The kungfu values (FW-MH.kungfu, D-Y18E1): one value for each ruleset that owns tools, and the
//! line that delivers it. The tool table says which ruleset owns each tool. `build.rs` checks the
//! table and the values, then compiles the table in, so nothing here reads a disk or a network.
//!
//! The nine values and their lines are derived from the checked guide once per process. The raw
//! guide rows stay as they are: a value hashes them, and [`Owner::delivered`] adds the line only
//! when a row is copied into a reply.
use crate::embedded_guide;
use std::sync::OnceLock;

#[allow(dead_code)]
#[path = "../build/kungfu.rs"]
mod derive;

pub use derive::{Response, VALUE_BYTES};

/// One tool's row of the table. A `None` ruleset means the tool is exempt: no check and no count.
#[derive(Debug, PartialEq, Eq)]
pub struct Policy {
    pub tool: &'static str,
    /// The owner's load handle, such as `rdy:write`.
    pub ruleset: Option<&'static str>,
    pub response: Response,
}

include!(concat!(env!("OUT_DIR"), "/kungfu_table.rs"));

/// The row of `tool`. The table is in tool-name order.
pub fn policy(tool: &str) -> Option<&'static Policy> {
    TOOLS
        .binary_search_by(|p| p.tool.cmp(tool))
        .ok()
        .map(|i| &TOOLS[i])
}

/// A ruleset that owns tools: its current value and the line that carries it.
#[derive(Debug, PartialEq, Eq)]
pub struct Owner {
    /// The ruleset's name, with no `rdy:` prefix.
    pub name: &'static str,
    pub value: String,
    pub suffix: String,
}

impl Owner {
    /// `text`, this owner's final source row, as a reply delivers it.
    pub fn delivered(&self, text: &str) -> String {
        derive::delivered(text, &self.suffix)
    }

    /// The bytes [`Owner::delivered`] adds to `text`.
    pub fn added_bytes(&self, text: &str) -> u64 {
        derive::added_bytes(text, &self.suffix) as u64
    }
}

static CACHE: OnceLock<Vec<Owner>> = OnceLock::new();

/// The nine owners in delivery order, derived on the first call of the process.
pub fn owners() -> &'static [Owner] {
    CACHE.get_or_init(derive_owners)
}

/// The owner named `name`, with no `rdy:` prefix. A ruleset that owns no tool has none.
pub fn owner(name: &str) -> Option<&'static Owner> {
    owners().iter().find(|o| o.name == name)
}

fn derive_owners() -> Vec<Owner> {
    #[cfg(feature = "test-support")]
    DERIVATIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    embedded_guide::RULESETS
        .iter()
        .filter(|set| derive::OWNERS.contains(&set.name))
        .map(|set| {
            let handle = format!("rdy:{}", set.name);
            let tools: Vec<&str> = TOOLS
                .iter()
                .filter(|p| p.ruleset == Some(handle.as_str()))
                .map(|p| p.tool)
                .collect();
            let value = derive::value_of(set.rows.iter().map(|r| r.text));
            let suffix = derive::suffix_of(&value, &tools);
            Owner {
                name: set.name,
                value,
                suffix,
            }
        })
        .collect()
}

#[cfg(feature = "test-support")]
static DERIVATIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many times this process derived the owners.
#[cfg(feature = "test-support")]
pub fn derivations_for_tests() -> usize {
    DERIVATIONS.load(std::sync::atomic::Ordering::SeqCst)
}
