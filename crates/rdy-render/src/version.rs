//! The site's wording of where a page stands among the versions (Task 12 words it).
use rdy_engine::VersionPosition;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Label {
    pub text: String,
    pub frozen: bool,
}

/// The version pill's text: the engine's label until Task 12.
pub fn label(p: &VersionPosition) -> Label {
    Label {
        text: p.label.clone(),
        frozen: p.frozen,
    }
}
