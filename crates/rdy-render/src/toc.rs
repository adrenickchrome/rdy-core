//! The "On this page" list (Task 6 fills it).
use crate::page::Item;
use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Toc {
    pub entries: Vec<TocEntry>,
}

// Task 6 gives an entry its fields
#[allow(dead_code)]
#[derive(Clone, Debug, Serialize)]
pub struct TocEntry {}

pub fn build(_items: &[Item]) -> Toc {
    Toc::default()
}
