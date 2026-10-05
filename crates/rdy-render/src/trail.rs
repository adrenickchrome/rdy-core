//! The "Why ?" trail of a block, the agents' affordance and the copied reference (Task 11 fills it).
use crate::site::Site;
use rdy_engine::Materialized;
use rdy_graph::Graph;
use rdy_model::{Block, BlockId};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Trail {}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Agents {}

pub fn for_block(_b: &Block, _g: &Graph, _m: &Materialized, _site: &Site, _from: &str) -> Trail {
    Trail::default()
}

pub fn agents(
    _b: &Block,
    _g: &Graph,
    _m: &Materialized,
    _suspects: &BTreeSet<BlockId>,
    _site: &Site,
    _from: &str,
    _json_href: &str,
) -> Agents {
    Agents::default()
}

/// The two `data-ref` attributes of a block's wrapper: the head (`[id]` or `[id@version]`) and
/// the whole line. `pin` is the version name when the page is rendered from a tag.
pub fn ref_attrs(
    _b: &Block,
    _site: &Site,
    _pin: Option<&str>,
    _m: &Materialized,
) -> (String, String) {
    (String::new(), String::new())
}
