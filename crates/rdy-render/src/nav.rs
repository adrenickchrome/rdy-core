//! The left navigation (Task 6 fills it).
use crate::site::Site;
use rdy_model::BlockId;
use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Nav {
    pub groups: Vec<NavGroup>,
}

// Task 6 gives a group its fields
#[allow(dead_code)]
#[derive(Clone, Debug, Serialize)]
pub struct NavGroup {}

/// The navigation as page `from` shows it, `current` being the block the page is for.
pub fn build(
    _site: &Site,
    _current: Option<&BlockId>,
    _from: &str,
    _has_terms: bool,
    _has_questions: bool,
) -> Nav {
    Nav::default()
}
