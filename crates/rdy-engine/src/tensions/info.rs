//! What a surface shows of a tension: its id and label, kind, severity, status, sides (with each
//! side's views and provenance) and blocks.
use super::{SideRef, Tension, TensionKind, TensionSeverity, tension_label};
use crate::ReadState;
use crate::coord_graph::intent_label;
use crate::history::role_str;
use crate::session::rfc3339;
use rdy_model::Hash;
use serde::Serialize;

/// Whether a tension is open or dismissed (by the review named).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TensionStatus {
    Open,
    Dismissed(Hash),
}

/// A tension as the surfaces list it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TensionInfo {
    /// `tn-<12 hex>`.
    pub id: String,
    /// The full id, 64 hex.
    pub tension: String,
    pub kind: TensionKind,
    pub severity: TensionSeverity,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub overridden: bool,
    /// `open` or `dismissed`.
    pub status: String,
    pub summary: String,
    pub sides: Vec<SideInfo>,
    pub blocks: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dismissal: Option<DismissalInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refuses: Option<bool>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub read_time: bool,
}

/// One side of a tension: its label, what it is (a change, a landing, an intent), the open views
/// that hold it, who wrote it, and the blocks it involves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SideInfo {
    pub side: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landing: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    pub views: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    pub blocks: Vec<SideBlock>,
}

/// A block a side involves, at its version (`None`: deleted or absent).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SideBlock {
    pub block: String,
    pub version: Option<String>,
}

/// The review that dismissed a tension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DismissalInfo {
    pub review: String,
    pub actor: String,
    pub run: Option<String>,
    pub role: String,
    pub at: String,
    pub at_ms: i64,
    pub reason: String,
}

impl ReadState {
    /// Whether `t` is open or dismissed in this state (a dismissal binds to the vkey).
    pub fn tension_status(&self, t: &Tension) -> TensionStatus {
        match self.dismissals.get(&t.vkey) {
            Some(review) => TensionStatus::Dismissed(*review),
            None => TensionStatus::Open,
        }
    }

    /// `t` as the surfaces list it. A change side's views are the open views that hold it and
    /// its actor and run the change's; a canon side's actor and run are those of the change that
    /// produced its involved version; an intent side's are the intent's. A dismissed tension
    /// carries its review. What cannot be read is left out.
    pub fn tension_info(&self, t: &Tension) -> TensionInfo {
        let (status, dismissal) = match self.tension_status(t) {
            TensionStatus::Open => ("open", None),
            TensionStatus::Dismissed(review) => ("dismissed", self.dismissal_info(&review)),
        };
        TensionInfo {
            id: tension_label(&t.id),
            tension: t.id.to_string(),
            kind: t.kind,
            severity: t.severity,
            overridden: t.overridden,
            status: status.to_string(),
            summary: t.summary.clone(),
            sides: t.sides.iter().map(|s| self.side_info(t, s)).collect(),
            blocks: t.blocks().iter().map(ToString::to_string).collect(),
            dismissal,
            refuses: None,
            read_time: t.read_time,
        }
    }

    /// The dismissal review `review` as the surfaces show it, if it can be read.
    fn dismissal_info(&self, review: &Hash) -> Option<DismissalInfo> {
        let r = self.reader().review(review).ok()?;
        let p = r.provenance;
        Some(DismissalInfo {
            review: review.to_string(),
            actor: p.actor,
            run: p.run,
            role: role_str(p.role).to_string(),
            at: rfc3339(p.at_ms),
            at_ms: p.at_ms,
            reason: r.reason,
        })
    }

    fn side_info(&self, t: &Tension, side: &SideRef) -> SideInfo {
        let items: Vec<_> = t.involved.iter().filter(|i| i.side == *side).collect();
        let mut info = SideInfo {
            side: side.label(),
            change: None,
            landing: None,
            intent: None,
            views: vec![],
            actor: None,
            run: None,
            blocks: items
                .iter()
                .map(|i| SideBlock {
                    block: i.block.to_string(),
                    version: i.version.map(|v| v.to_string()),
                })
                .collect(),
        };
        let reader = self.reader();
        let provenance = |c: &Hash| reader.change(c).ok().map(|ch| ch.provenance);
        match side {
            SideRef::Change(c) => {
                info.change = Some(c.to_string());
                if let Ok(set) = self.inflight_set() {
                    info.views = set.views_of.get(c).cloned().unwrap_or_default();
                }
                if let Some(p) = provenance(c) {
                    (info.actor, info.run) = (Some(p.actor), p.run);
                }
            }
            SideRef::Canon { landing } => {
                info.landing = landing.map(|l| l.to_string());
                let producer = items
                    .iter()
                    .find_map(|i| i.version)
                    .and_then(|v| reader.producer(&v).ok().flatten());
                if let Some(p) = producer.as_ref().and_then(provenance) {
                    (info.actor, info.run) = (Some(p.actor), p.run);
                }
            }
            SideRef::Intent { stable, version } => {
                info.intent = Some(intent_label(stable));
                if let Ok(it) = reader.intent(version) {
                    info.views = vec![it.view];
                    (info.actor, info.run) = (Some(it.actor), it.run);
                }
            }
        }
        info
    }
}
