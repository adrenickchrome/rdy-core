//! The search index (Task 7 fills it).
use crate::page::PageContext;
use crate::site::Site;
use rdy_engine::{E_IO, Materialized, RdyError};
use std::path::Path;

#[derive(Default)]
pub struct Index {}

/// Adds the page's entries to the index.
pub fn collect(_index: &mut Index, _page: &PageContext, _m: &Materialized, _site: &Site) {}

/// Writes `search-index.json` under `out`.
pub fn write(_index: &Index, site_name: &str, out: &Path) -> Result<(), RdyError> {
    let site = serde_json::to_string(site_name).expect("json");
    std::fs::write(
        out.join("search-index.json"),
        format!("{{\"version\":1,\"site\":{site},\"entries\":[]}}"),
    )
    .map_err(|e| {
        RdyError::new(
            E_IO,
            format!("cannot write the search index: {e}"),
            "check the output directory",
        )
    })
}
