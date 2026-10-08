//! arn1: the areas a block is in. A block is in the areas its doc's path matches (a doc or folder:
//! its own path); a decision is also in the areas of the blocks its `affects` links target.
use crate::error::*;
use crate::query::ImpactItem;
use crate::{Materialized, ReadState, Scope};
use rdy_model::decision::{AFFECTS, DECISION};
use rdy_model::{Block, BlockId};
use rdy_schema::AreaMatcher;
use std::collections::{BTreeMap, BTreeSet};

/// The area names of the path of `id` in `m`, adding to `out`. A block outside any doc has no path.
fn add_path_areas(
    matcher: &AreaMatcher,
    m: &Materialized,
    id: &BlockId,
    out: &mut BTreeSet<String>,
) {
    if let Some(path) = m.path_of(id) {
        out.extend(
            matcher
                .areas_for_path(&path)
                .into_iter()
                .map(str::to_string),
        );
    }
}

impl ReadState {
    /// The pack's areas, compiled once when the pack was loaded.
    pub fn area_matcher(&self) -> &AreaMatcher {
        &self.areas
    }

    /// The areas `id` is in, in `scope`: ascending, deduplicated; `E_UNKNOWN_BLOCK` for an absent
    /// block. Every block is in the areas its doc's path matches. A decision is also in the areas
    /// of the blocks its `affects` links target. (The areas its `affects` links name directly join
    /// when `affects` can target an area node: no block is an area node yet.)
    pub fn areas_of(&self, scope: &Scope, id: &BlockId) -> Result<Vec<String>, RdyError> {
        let block = self.block(scope, id)?;
        let m = self.materialized(scope)?;
        Ok(self.areas_in(&m, &block))
    }

    /// [`ReadState::areas_of`] for a block already in hand, in its materialization `m`. Nothing is
    /// computed for a pack without areas.
    pub(crate) fn areas_in(&self, m: &Materialized, block: &Block) -> Vec<String> {
        if self.areas.is_empty() {
            return Vec::new();
        }
        let mut out = BTreeSet::new();
        add_path_areas(&self.areas, m, &block.id, &mut out);
        if block.block_type == DECISION {
            for link in block.links.get(AFFECTS).into_iter().flatten() {
                add_path_areas(&self.areas, m, &link.target, &mut out);
            }
        }
        out.into_iter().collect()
    }
}

/// How many of `items` are in each area; areas with no item are absent.
pub fn by_area(items: &[ImpactItem]) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for a in items.iter().flat_map(|i| &i.areas) {
        *out.entry(a.clone()).or_insert(0) += 1;
    }
    out
}
