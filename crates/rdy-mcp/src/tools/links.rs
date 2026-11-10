//! `rdy_links`: the repo links of the project and what they deliver (repo links, D-DAPR8). It
//! reads canon and the foreign copy and writes nothing; the engine's `links`, `link_show` and
//! `link_refs` do the work, and the CLI's `rdy links` reads the same.

use super::*;
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// The ops of `rdy_links`, as its refusal names them.
pub const LINKS_OPS: &str = "list, show, refs";

/// The fix of an `op` this tool does not have.
const LINKS_OP_FIX: &str = "use op list, show or refs";

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinksParams {
    /// list (default): every repo link with its state, and the delivered repos no link names. show: what the link `alias` delivers and the instructions it has not adopted; needs alias. refs: the pins of canon to cited content, by repo; alias limits it to that link's repo.
    pub op: Option<String>,
    /// The alias of a repo link. Required by show; optional with refs; refused with list.
    pub alias: Option<String>,
}

fn bad(what: impl Into<String>, fix: impl Into<String>) -> RdyError {
    RdyError::new(E_BAD_INPUT, what, fix)
}

fn json<T: serde::Serialize>(v: T) -> Result<Value, RdyError> {
    serde_json::to_value(v).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a links reply did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })
}

/// `rdy_links`: the links, one link's delivery, or the pins to cited content.
pub fn rdy_links(st: &mut McpState, p: LinksParams) -> Result<Value, RdyError> {
    session(st)?;
    let op = p.op.as_deref().unwrap_or("list");
    let st_ = st.engine.read();
    match (op, p.alias.as_deref()) {
        ("list", None) => json(st_.links()?),
        ("list", Some(_)) => Err(bad(
            "list takes no alias",
            "pass op show or refs with alias, or leave alias out",
        )),
        ("show", Some(alias)) => json(st_.link_show(alias)?),
        ("show", None) => Err(bad(
            "show needs an alias",
            "pass the alias of a repo link: rdy_links lists them",
        )),
        ("refs", alias) => json(st_.link_refs(alias)?),
        (other, _) => Err(bad(
            format!("unknown op {other:?}"),
            format!("{LINKS_OP_FIX}: {LINKS_OPS}"),
        )),
    }
}
