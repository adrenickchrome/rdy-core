//! The site's wording of where a page stands among the versions (rule 2.11). The engine's own
//! label (`VersionPosition.label`, "v1.2.0 + 3 landings") is storage wording and stays with the
//! CLI and MCP.
use rdy_engine::VersionPosition;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Label {
    pub text: String,
    pub frozen: bool,
}

fn count(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The version pill's text: `unversioned`, `v1.2.0`, `v1.2.0, 3 changes since`, then, for a
/// view, ` · draft: {view}, {n} pending changes`. A frozen scope reads the same and says
/// `frozen` through the flag.
pub fn label(p: &VersionPosition) -> Label {
    let mut text = match &p.version {
        None => "unversioned".to_string(),
        Some(v) if p.landings_since == 0 => v.name.clone(),
        Some(v) => format!(
            "{}, {} since",
            v.name,
            count(p.landings_since, "change", "changes")
        ),
    };
    if let Some(v) = &p.view {
        text.push_str(&format!(
            " · draft: {}, {}",
            v.name,
            count(v.unlanded_changes, "pending change", "pending changes")
        ));
    }
    Label {
        text,
        frozen: p.frozen,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_engine::{VersionRef, ViewPosition};

    fn pos(
        version: Option<&str>,
        since: u32,
        view: Option<(&str, u32)>,
        frozen: bool,
    ) -> VersionPosition {
        VersionPosition {
            version: version.map(|n| VersionRef {
                name: n.into(),
                landing: "0".repeat(64),
            }),
            landings_since: since,
            view: view.map(|(n, c)| ViewPosition {
                name: n.into(),
                unlanded_changes: c,
            }),
            frozen,
            label: "storage wording".into(),
        }
    }

    #[test]
    fn ver1_the_label_in_site_wording() {
        let t = |p: VersionPosition| label(&p).text;
        assert_eq!(t(pos(None, 0, None, false)), "unversioned");
        assert_eq!(t(pos(None, 4, None, false)), "unversioned");
        assert_eq!(t(pos(Some("v1.2.0"), 0, None, false)), "v1.2.0");
        assert_eq!(
            t(pos(Some("v1.2.0"), 3, None, false)),
            "v1.2.0, 3 changes since"
        );
        assert_eq!(
            t(pos(Some("v1.2.0"), 1, None, false)),
            "v1.2.0, 1 change since"
        );
        assert_eq!(
            t(pos(Some("v1.2.0"), 3, Some(("work", 2)), false)),
            "v1.2.0, 3 changes since · draft: work, 2 pending changes"
        );
        assert_eq!(
            t(pos(Some("v1.2.0"), 0, Some(("work", 1)), false)),
            "v1.2.0 · draft: work, 1 pending change"
        );
        assert_eq!(
            t(pos(None, 0, Some(("work", 0)), false)),
            "unversioned · draft: work, 0 pending changes"
        );
    }

    #[test]
    fn ver1_a_frozen_scope_is_flagged_and_keeps_the_wording() {
        let l = label(&pos(Some("v1.0.0"), 0, None, true));
        assert_eq!((l.text.as_str(), l.frozen), ("v1.0.0", true));
        let l = label(&pos(Some("v1.0.0"), 3, None, true));
        assert_eq!(
            (l.text.as_str(), l.frozen),
            ("v1.0.0, 3 changes since", true)
        );
        assert!(!label(&pos(Some("v1.0.0"), 0, None, false)).frozen);
    }
}
