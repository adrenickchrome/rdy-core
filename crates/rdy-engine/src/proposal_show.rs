//! Reading one proposal version (`P-<id>@<n>`): its title, proposer, changes with block counts,
//! what blocks approving it now, and its version chain. `proposal_text` renders the JSON form
//! of a [`ProposalShow`] for the command line and for MCP.

use crate::gate::{
    RelayedByRow, blocking_conflicts, changes_of, landed_as, latest_version, resolve_in,
    version_number,
};
use crate::session::rfc3339;
use crate::tensions::info::FindingInfo;
use crate::tensions::tension_label;
use crate::views::{ConflictInfo, conflict_infos};
use crate::{RdyError, ReadState};
use rdy_model::Hash;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

/// One change of a proposal version.
#[derive(Clone, Debug, Serialize)]
pub struct ProposalChangeRow {
    /// The change's full hash.
    pub change: String,
    pub message: String,
    /// The distinct block ids its ops touch.
    pub blocks: usize,
}

/// One version in a proposal's chain.
#[derive(Clone, Debug, Serialize)]
pub struct ProposalVersionRow {
    /// `P-<stable short>@<n>`.
    pub r#ref: String,
    pub id: String,
    /// Only the last version of the chain.
    pub latest: bool,
    /// Any of its changes landed (the gate's `landed_as` test): the version a human approved and
    /// the final version a landing wrote after it both read landed.
    pub landed: bool,
}

/// A proposal version as a read shows it.
#[derive(Clone, Debug, Serialize)]
pub struct ProposalShow {
    /// `P-<stable short>@<version>`.
    pub r#ref: String,
    /// The version's hash.
    pub id: String,
    /// The proposal's stable id (its first version's hash).
    pub stable: String,
    pub version: u32,
    pub title: String,
    pub summary: Option<String>,
    pub view: String,
    pub proposed_by: RelayedByRow,
    /// RFC 3339 UTC.
    pub proposed_at: String,
    /// Canon's head when this version was written.
    pub canon_at: Option<String>,
    /// `landed` if any of its changes landed, else `superseded` if a newer version exists, else
    /// `open`.
    pub state: String,
    /// The landing that carried this version, when `state` is `landed`.
    pub landing: Option<String>,
    pub changes: Vec<ProposalChangeRow>,
    /// Distinct block ids over all changes.
    pub blocks: usize,
    /// The conflicts on blocks this version writes; computed only when `open`.
    pub conflicts: Vec<ConflictInfo>,
    /// What refuses approving this version now, one line each.
    pub blocking: Vec<String>,
    /// The chain, oldest first.
    pub versions: Vec<ProposalVersionRow>,
    /// The semantic tensions of this version whose reviewer advised blocking, by tension id;
    /// read only when `open` (D-WMCFW).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub advised: Vec<AdvisedRow>,
    /// `<code>: <message>` when the tensions could not be read; the rest of the show stands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advised_error: Option<String>,
}

/// An advised semantic tension of a proposal version: the reviewer, the advice and findings,
/// and whether a person accepted the advice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AdvisedRow {
    /// `tn-<12 hex>`.
    pub tension: String,
    pub reviewer: String,
    pub advice: String,
    pub findings: Vec<FindingInfo>,
    pub accepted: bool,
}

/// Whether `text` is a proposal ref: `P-`, 4 to 64 hex digits, then nothing or `@` and digits.
pub fn is_proposal_ref(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("P-") else {
        return false;
    };
    let (id, n) = match rest.split_once('@') {
        Some((id, n)) => (id, Some(n)),
        None => (rest, None),
    };
    (4..=64).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_hexdigit())
        && n.is_none_or(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

impl ReadState {
    /// Reads the proposal version `text` names (`P-<id>` is its latest version).
    pub fn proposal_show(&self, text: &str) -> Result<ProposalShow, RdyError> {
        let pref = resolve_in(self, text)?;
        let reader = self.reader();
        let p = reader.proposal(&pref.version)?;
        let latest = latest_version(self, &pref.stable)?;
        let latest_p = if latest == pref.version {
            p.clone()
        } else {
            reader.proposal(&latest)?
        };
        // The chain, oldest first, each version with the gate's own landed test.
        let mut chain = vec![(latest, landed_as(self, &latest_p)?.is_some())];
        let mut cur = latest_p.prev;
        while let Some(h) = cur {
            let v = reader.proposal(&h)?;
            chain.push((h, landed_as(self, &v)?.is_some()));
            cur = v.prev;
        }
        chain.reverse();
        let stable_short = pref.stable.short();
        let versions: Vec<ProposalVersionRow> = chain
            .iter()
            .enumerate()
            .map(|(i, (h, landed))| ProposalVersionRow {
                r#ref: format!("P-{stable_short}@{}", i + 1),
                id: h.to_string(),
                latest: *h == latest,
                landed: *landed,
            })
            .collect();
        // landed first, as the gate refuses it (E_ALREADY_LANDED): approving a version that
        // carries a decision lands a later, final version, and the approved one has landed too
        let (state, landing) = if let Some(l) = landed_as(self, &p)? {
            ("landed", Some(l))
        } else if pref.version != latest {
            ("superseded", None)
        } else {
            ("open", None)
        };
        let cs = changes_of(self, &p.changes)?;
        let mut all: BTreeSet<String> = BTreeSet::new();
        let changes: Vec<ProposalChangeRow> = cs
            .iter()
            .zip(&p.changes)
            .map(|(c, h)| {
                let own: BTreeSet<String> =
                    c.ops.iter().map(|op| op.block_id().to_string()).collect();
                all.extend(own.iter().cloned());
                ProposalChangeRow {
                    change: h.to_string(),
                    message: c.message.clone(),
                    blocks: own.len(),
                }
            })
            .collect();
        let conflicts = if state == "open" {
            conflict_infos(self, &blocking_conflicts(self, &p.view, &p.changes)?)?
        } else {
            vec![]
        };
        let mut blocking = vec![];
        if state == "superseded" {
            let n = version_number(self, &latest)?;
            blocking.push(format!("superseded by P-{stable_short}@{n}"));
        }
        if let Some(l) = &landing {
            blocking.push(format!("landed in {}", l.short()));
        }
        if !conflicts.is_empty() {
            blocking.push(format!(
                "{} unresolved conflict(s) on blocks it writes",
                conflicts.len()
            ));
        }
        let (advised, advised_error) = if state == "open" {
            match self.advised_rows(&p.changes) {
                Ok(rows) => (rows, None),
                Err(e) => (vec![], Some(format!("{}: {}", e.code, e.message))),
            }
        } else {
            (vec![], None)
        };
        Ok(ProposalShow {
            r#ref: format!("P-{stable_short}@{}", pref.n),
            id: pref.version.to_string(),
            stable: pref.stable.to_string(),
            version: pref.n,
            title: p.title.clone(),
            summary: p.summary.clone(),
            view: p.view.clone(),
            proposed_by: RelayedByRow {
                actor: p.provenance.actor.clone(),
                run: p.provenance.run.clone(),
            },
            proposed_at: rfc3339(p.provenance.at_ms),
            canon_at: p.canon_at.as_ref().map(Hash::to_string),
            state: state.to_string(),
            landing: landing.map(|l| l.to_string()),
            changes,
            blocks: all.len(),
            conflicts,
            blocking,
            versions,
            advised,
            advised_error,
        })
    }

    /// The advised semantic tensions of `changes` as `rdy show P-x@n` lists them: the reviewer
    /// and findings from each verdict's review.
    fn advised_rows(&self, changes: &[Hash]) -> Result<Vec<AdvisedRow>, RdyError> {
        self.advised_of(changes)?
            .into_iter()
            .map(|a| {
                let rec = &a.open.record;
                let r = self.reader().review(&rec.review)?;
                Ok(AdvisedRow {
                    tension: tension_label(&a.tension.id),
                    reviewer: r.provenance.actor,
                    advice: rec.advice.clone().unwrap_or_default(),
                    findings: r
                        .findings
                        .into_iter()
                        .map(|f| FindingInfo {
                            block: f.block.to_string(),
                            note: f.note,
                        })
                        .collect(),
                    accepted: rec.blocking,
                })
            })
            .collect()
    }
}

/// The text of `s`, a serialized [`ProposalShow`]: the command line renders the daemon's JSON
/// and MCP renders its own.
pub fn proposal_text(v: &Value) -> String {
    let s = |x: &Value| x.as_str().unwrap_or("").to_string();
    let first12 = |x: &Value| s(x).chars().take(12).collect::<String>();
    let mut out = format!(
        "proposal {} (version {}): {}\n",
        s(&v["ref"]),
        first12(&v["id"]),
        s(&v["title"])
    );
    if let Some(summary) = v["summary"].as_str().filter(|t| !t.is_empty()) {
        out.push_str(&format!("summary: {summary}\n"));
    }
    let run = v["proposed_by"]["run"]
        .as_str()
        .map(|r| format!("/{r}"))
        .unwrap_or_default();
    out.push_str(&format!(
        "view {}, proposed by {}{run} at {}\n",
        s(&v["view"]),
        s(&v["proposed_by"]["actor"]),
        s(&v["proposed_at"])
    ));
    match v["state"].as_str().unwrap_or("") {
        "landed" => out.push_str(&format!("state: landed in {}\n", first12(&v["landing"]))),
        state => out.push_str(&format!("state: {state}\n")),
    }
    let changes = v["changes"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    out.push_str(&format!(
        "changes: {} ({} blocks)\n",
        changes.len(),
        v["blocks"].as_u64().unwrap_or(0)
    ));
    for c in changes {
        let message = c["message"].as_str().filter(|m| !m.is_empty());
        out.push_str(&format!(
            "  {} {} block(s): {}\n",
            first12(&c["change"]),
            c["blocks"].as_u64().unwrap_or(0),
            message.unwrap_or("(no message)")
        ));
    }
    for c in v["conflicts"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "conflict {} on {}\n",
            first12(&c["id"]),
            s(&c["block"])
        ));
    }
    let versions: Vec<String> = v["versions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let mut t = s(&r["ref"]);
            if r["latest"].as_bool() == Some(true) {
                t.push_str(" (latest)");
            }
            if r["landed"].as_bool() == Some(true) {
                t.push_str(" (landed)");
            }
            t
        })
        .collect();
    out.push_str(&format!("versions: {}\n", versions.join(", ")));
    let blocking: Vec<String> = v["blocking"]
        .as_array()
        .into_iter()
        .flatten()
        .map(&s)
        .collect();
    if blocking.is_empty() {
        out.push_str(&format!("approve with: rdy approve {}\n", s(&v["ref"])));
    } else {
        out.push_str(&format!("blocking: {}\n", blocking.join("; ")));
    }
    for a in v["advised"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "advised: {} by {}: {}{}\n",
            s(&a["tension"]),
            s(&a["reviewer"]),
            s(&a["advice"]),
            if a["accepted"].as_bool() == Some(true) {
                " (accepted)"
            } else {
                ""
            }
        ));
    }
    if let Some(e) = v["advised_error"].as_str() {
        out.push_str(&format!("advised: not read ({e})\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pshow1_proposal_text_is_exact() {
        let h = |c: char| c.to_string().repeat(64);
        let mut v = json!({
            "ref": "P-111111111111@2", "id": h('2'), "stable": h('1'), "version": 2,
            "title": "Pins", "summary": "Pins record content.", "view": "work",
            "proposed_by": {"actor": "agent:a", "run": "r1"}, "proposed_at": "2026-10-06T10:00:00Z",
            "canon_at": null, "state": "open", "landing": null,
            "changes": [
                {"change": h('3'), "message": "pin content", "blocks": 2},
                {"change": h('4'), "message": "", "blocks": 1}
            ],
            "blocks": 3,
            "conflicts": [{"id": h('5'), "block": "c-AAAAA", "base": null, "sides": []}],
            "blocking": ["1 unresolved conflict(s) on blocks it writes"],
            "versions": [
                {"ref": "P-111111111111@1", "id": h('1'), "latest": false, "landed": false},
                {"ref": "P-111111111111@2", "id": h('2'), "latest": true, "landed": false}
            ]
        });
        assert_eq!(
            proposal_text(&v),
            "proposal P-111111111111@2 (version 222222222222): Pins\n\
             summary: Pins record content.\n\
             view work, proposed by agent:a/r1 at 2026-10-06T10:00:00Z\n\
             state: open\n\
             changes: 2 (3 blocks)\n\
             \x20 333333333333 2 block(s): pin content\n\
             \x20 444444444444 1 block(s): (no message)\n\
             conflict 555555555555 on c-AAAAA\n\
             versions: P-111111111111@1, P-111111111111@2 (latest)\n\
             blocking: 1 unresolved conflict(s) on blocks it writes\n"
        );
        v["summary"] = json!(null);
        v["proposed_by"]["run"] = json!(null);
        v["state"] = json!("landed");
        v["landing"] = json!(h('6'));
        v["conflicts"] = json!([]);
        v["blocking"] = json!([]);
        v["versions"][1]["landed"] = json!(true);
        let t = proposal_text(&v);
        assert!(t.starts_with("proposal P-111111111111@2 (version 222222222222): Pins\nview work, proposed by agent:a at "), "{t}");
        assert!(t.contains("state: landed in 666666666666\n"), "{t}");
        assert!(
            t.contains("versions: P-111111111111@1, P-111111111111@2 (latest) (landed)\n"),
            "{t}"
        );
        assert!(
            t.ends_with("approve with: rdy approve P-111111111111@2\n"),
            "{t}"
        );
    }

    /// D-WMCFW: `rdy show P-x@n` names each advised tension after the blocking line, marks an
    /// accepted one, and says when the tensions could not be read.
    #[test]
    fn se8_proposal_text_names_the_advised_tensions() {
        let h = |c: char| c.to_string().repeat(64);
        let mut v = json!({
            "ref": "P-111111111111@1", "id": h('1'), "stable": h('1'), "version": 1,
            "title": "T", "summary": null, "view": "a",
            "proposed_by": {"actor": "agent:a", "run": null}, "proposed_at": "2026-10-06T10:00:00Z",
            "canon_at": null, "state": "open", "landing": null, "changes": [], "blocks": 0,
            "conflicts": [], "blocking": [],
            "versions": [{"ref": "P-111111111111@1", "id": h('1'), "latest": true, "landed": false}],
            "advised": [
                {"tension": "tn-aaaaaaaaaaaa", "reviewer": "agent:r", "advice": "no", "findings": [], "accepted": false},
                {"tension": "tn-bbbbbbbbbbbb", "reviewer": "agent:s", "advice": "yes", "findings": [], "accepted": true}
            ]
        });
        assert!(proposal_text(&v).ends_with(
            "approve with: rdy approve P-111111111111@1\n\
             advised: tn-aaaaaaaaaaaa by agent:r: no\n\
             advised: tn-bbbbbbbbbbbb by agent:s: yes (accepted)\n"
        ));
        v["advised"] = json!([]);
        v["advised_error"] = json!("E_IO: busy");
        assert!(proposal_text(&v).ends_with("advised: not read (E_IO: busy)\n"));
    }
}
