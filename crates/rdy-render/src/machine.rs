//! The machine-readable twins of the pages and `llms.txt` (Task 13 fills it).
use crate::page::PageContext;
use crate::site::Site;
use rdy_engine::{E_IO, Materialized, RdyError, VersionPosition};
use rdy_graph::Graph;
use rdy_model::BlockId;
use rdy_schema::Pack;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;

pub fn page_json(
    _ctx: &PageContext,
    _m: &Materialized,
    _g: &Graph,
    _suspects: &BTreeSet<BlockId>,
    _pos: &VersionPosition,
    _site: &Site,
    _pack: &Pack,
) -> Value {
    json!({"schema": "rdy-page/1"})
}

/// Writes `llms.txt` under `out`.
pub fn write_index(
    out: &Path,
    _pages: &[PageContext],
    site_name: &str,
    _description: Option<&str>,
    _version_text: &str,
) -> Result<(), RdyError> {
    std::fs::write(out.join("llms.txt"), format!("# {site_name}\n")).map_err(|e| {
        RdyError::new(
            E_IO,
            format!("cannot write llms.txt: {e}"),
            "check the output directory",
        )
    })
}
