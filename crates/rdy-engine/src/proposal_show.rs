//! Reading one proposal version (`P-<id>@<n>`): its title, proposer, changes with block counts,
//! what blocks approving it now, and its version chain. `proposal_text` renders the JSON form
//! of a [`ProposalShow`] for the command line and for MCP.

use crate::gate::{
    RelayedByRow, blocking_conflicts, changes_of, landed_as, latest_version, resolve_in,
    version_number, withdrawn,
};
use crate::pending::queue::removal_item;
use crate::pending::{Item, RemovalCard};
use crate::semantic::accept_advice::advised_digest;
use crate::session::rfc3339;
use crate::tensions::info::CitationInfo;
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
    /// `withdrawn` if its view closed after it was written or is closed now (D-K0SS1), else
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
    /// The digest of `advised`, which an acceptance passes back (as the propose reply's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advised_digest: Option<String>,
    /// The approval card of this version when it is `open` or `withdrawn` and a change of it
    /// carries a content removal request (D-CC90E): read for this version alone, whatever the
    /// queue lists. A direct approval (`rdy approve`) shows it before the approval runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removal_card: Option<RemovalCardShow>,
    /// Why the removal card could not be read (`E_REDACTION_BUDGET` when its mandatory lines do
    /// not fit [`crate::pending::CARD_MAX_TOKENS`]); the rest of the show stands, and a direct
    /// approval refuses with this error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removal_card_error: Option<ErrorRow>,
}

/// The approval card of a version that carries content removal: the queue item's ref, its card
/// whole, the card's hash and its removal facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RemovalCardShow {
    /// `P-<stable short>@<version>`.
    pub r#ref: String,
    pub card: String,
    pub card_hash: String,
    pub removal: Vec<RemovalCard>,
}

/// An error as a read carries it beside its other parts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ErrorRow {
    pub code: String,
    pub message: String,
    pub fix: String,
}

/// An advised semantic tension of a proposal version: the reviewer, the advice and citations,
/// and whether a person accepted the advice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AdvisedRow {
    /// `tn-<12 hex>`.
    pub tension: String,
    pub reviewer: String,
    pub advice: String,
    pub citations: Vec<CitationInfo>,
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

/// The spelling of `text` that `is_proposal_ref` (or the hash parser) recognizes, when the
/// approval's resolver reads `text` as a proposal; `None` when the resolver refuses `text` on
/// its syntax alone. It reads `text` exactly as `resolve_in` does: a full version hash as is,
/// else `P-`, 4 to 64 hex digits, then nothing or `@` and a version that parses as a `u32`
/// (which takes `@01` and `@+1`). The spelling it returns, `P-<digits>[@<n>]` with `n` in
/// decimal, resolves to what `text` resolves to. A direct approval reads the version's card by
/// it (rdx13), so no spelling the approval takes skips the card.
pub fn canonical_proposal_ref(text: &str) -> Option<String> {
    if text.parse::<Hash>().is_ok() {
        return Some(text.to_string());
    }
    let rest = text.strip_prefix("P-")?;
    let (prefix, n) = match rest.split_once('@') {
        Some((prefix, n)) => (prefix, Some(n.parse::<u32>().ok()?)),
        None => (rest, None),
    };
    if !((4..=64).contains(&prefix.len()) && prefix.bytes().all(|b| b.is_ascii_hexdigit())) {
        return None;
    }
    Some(match n {
        Some(n) => format!("P-{prefix}@{n}"),
        None => format!("P-{prefix}"),
    })
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
        } else if withdrawn(self, &pref.version, &p)? {
            ("withdrawn", None)
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
        if state == "withdrawn" {
            blocking.push(format!("withdrawn (view {} closed)", p.view));
        }
        if !conflicts.is_empty() {
            blocking.push(format!(
                "{} unresolved conflict(s) on blocks it writes",
                conflicts.len()
            ));
        }
        // a landed or superseded version never lands again (the approval refuses it before
        // anything else), so only an open version, or one a close withdrew (an undo can reopen
        // its view), has a removal card to show before an approval
        let (removal_card, removal_card_error) = if matches!(state, "open" | "withdrawn") {
            match removal_item(self, pref, p.clone()) {
                Ok(item) => (item.map(removal_card_show), None),
                Err(e) => (
                    None,
                    Some(ErrorRow {
                        code: e.code.to_string(),
                        message: e.message,
                        fix: e.fix,
                    }),
                ),
            }
        } else {
            (None, None)
        };
        let (advised, advised_digest, advised_error) = if state == "open" {
            match self.advised_rows(&p.view, &p.changes) {
                Ok((rows, digest)) => (rows, digest, None),
                Err(e) => (vec![], None, Some(format!("{}: {}", e.code, e.message))),
            }
        } else {
            (vec![], None, None)
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
            advised_digest,
            removal_card,
            removal_card_error,
        })
    }

    /// The advised semantic tensions of `changes` as `rdy show P-x@n` lists them, every one: the
    /// reviewer and citations from each verdict's review; and the digest of the set.
    fn advised_rows(
        &self,
        view: &str,
        changes: &[Hash],
    ) -> Result<(Vec<AdvisedRow>, Option<String>), RdyError> {
        let advised = self.advised_of(view, changes)?;
        let digest = advised_digest(&advised);
        let rows = advised
            .into_iter()
            .map(|a| {
                let rec = &a.open.record;
                let r = self.reader().review(&rec.review)?;
                Ok(AdvisedRow {
                    tension: tension_label(&a.tension.id),
                    reviewer: r.provenance.actor,
                    advice: rec.advice.clone().unwrap_or_default(),
                    citations: r
                        .citations
                        .into_iter()
                        .map(|f| CitationInfo {
                            block: f.block.to_string(),
                            note: f.note,
                        })
                        .collect(),
                    accepted: rec.blocking,
                })
            })
            .collect::<Result<Vec<_>, RdyError>>()?;
        Ok((rows, digest))
    }
}

/// The show of a removal item ([`removal_item`]).
fn removal_card_show(item: Item) -> RemovalCardShow {
    RemovalCardShow {
        r#ref: item.r#ref,
        card: item.card,
        card_hash: item.card_hash,
        removal: item.removal.unwrap_or_default(),
    }
}

/// The text of `s`, a serialized [`ProposalShow`]: the command line renders the daemon's JSON
/// and MCP renders its own. A version with a removal card shows the card whole and its card
/// hash before the approve or blocking line; a card that could not be read shows its error
/// row (code, message and fix) there instead.
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
    let by = crate::identity::actor_run_text(
        &s(&v["proposed_by"]["actor"]),
        v["proposed_by"]["run"].as_str(),
    );
    out.push_str(&format!(
        "view {}, proposed by {by} at {}\n",
        s(&v["view"]),
        s(&v["proposed_at"])
    ));
    match v["state"].as_str().unwrap_or("") {
        "landed" => out.push_str(&format!("state: landed in {}\n", first12(&v["landing"]))),
        "withdrawn" => out.push_str(&format!(
            "state: withdrawn (view {} closed)\n",
            s(&v["view"])
        )),
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
    // the removal card whole, the text `rdy pending` and `rdy approve` print (D-CC90E), so a
    // person sees what approving removes before the approve line
    if let Some(c) = v.get("removal_card").filter(|c| c.is_object()) {
        out.push_str(&format!(
            "\n{}\n\ncard hash: {}\n",
            s(&c["card"]),
            s(&c["card_hash"])
        ));
    }
    if let Some(e) = v.get("removal_card_error").filter(|e| e.is_object()) {
        out.push_str(&format!(
            "removal card: not read ({}: {}) (fix: {})\n",
            s(&e["code"]),
            s(&e["message"]),
            s(&e["fix"])
        ));
    }
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
    let open = v["advised"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|a| a.get("accepted").and_then(Value::as_bool) != Some(true));
    if let Some(d) = v["advised_digest"].as_str()
        && open
    {
        out.push_str(&format!(
            "accept the advice with: rdy resolve --accept {} --digest {d}\n",
            s(&v["ref"])
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
             view work, proposed by agent:a (run r1) at 2026-10-06T10:00:00Z\n\
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

    /// D-K0SS1: a version whose view closed reads `withdrawn (view <name> closed)`, says so on
    /// its blocking line, and offers no approve command.
    #[test]
    fn vw1_proposal_text_shows_a_withdrawn_version_and_no_approve_line() {
        let h = |c: char| c.to_string().repeat(64);
        let v = json!({
            "ref": "P-111111111111@2", "id": h('2'), "stable": h('1'), "version": 2,
            "title": "Pins", "summary": "Pins record content.", "view": "v",
            "proposed_by": {"actor": "agent:a", "run": "r1"}, "proposed_at": "2026-10-06T10:00:00Z",
            "canon_at": null, "state": "withdrawn", "landing": null,
            "changes": [
                {"change": h('3'), "message": "pin content", "blocks": 2},
                {"change": h('4'), "message": "", "blocks": 1}
            ],
            "blocks": 3,
            "conflicts": [],
            "blocking": ["withdrawn (view v closed)"],
            "versions": [
                {"ref": "P-111111111111@1", "id": h('1'), "latest": false, "landed": false},
                {"ref": "P-111111111111@2", "id": h('2'), "latest": true, "landed": false}
            ]
        });
        let t = proposal_text(&v);
        assert!(t.contains("state: withdrawn (view v closed)\n"), "{t}");
        assert!(t.ends_with("blocking: withdrawn (view v closed)\n"), "{t}");
        assert!(!t.contains("approve with:"), "{t}");
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
                {"tension": "tn-aaaaaaaaaaaa", "reviewer": "agent:r", "advice": "no", "citations": [], "accepted": false},
                {"tension": "tn-bbbbbbbbbbbb", "reviewer": "agent:s", "advice": "yes", "citations": [], "accepted": true}
            ],
            "advised_digest": "0123456789ab"
        });
        assert!(proposal_text(&v).ends_with(
            "approve with: rdy approve P-111111111111@1\n\
             advised: tn-aaaaaaaaaaaa by agent:r: no\n\
             advised: tn-bbbbbbbbbbbb by agent:s: yes (accepted)\n\
             accept the advice with: rdy resolve --accept P-111111111111@1 --digest 0123456789ab\n"
        ));
        // every advised tension accepted: nothing left to accept
        v["advised"][0]["accepted"] = json!(true);
        assert!(
            proposal_text(&v).ends_with("advised: tn-bbbbbbbbbbbb by agent:s: yes (accepted)\n")
        );
        v["advised"] = json!([]);
        v["advised_error"] = json!("E_IO: busy");
        assert!(proposal_text(&v).ends_with("advised: not read (E_IO: busy)\n"));
    }

    /// rdx13 (13.promoted fix round 1; ruling task-13-round1 item 2): the text of a show that
    /// carries `removal_card` prints that card whole, as `rdy approve` and `rdy pending` print it,
    /// then its card hash, before the approve or blocking line. A show that carries
    /// `removal_card_error` prints its code, message and fix there instead.
    #[test]
    fn rdx13_proposal_text_shows_the_removal_card_or_its_error() {
        let h = |c: char| c.to_string().repeat(64);
        let card = "P-111111111111@2  Pins                approve\n\nContent removal r-1\n  Reason class: secret.\n\nApproved content removal cannot be undone.";
        let mut v = json!({
            "ref": "P-111111111111@2", "id": h('2'), "stable": h('1'), "version": 2,
            "title": "Pins", "summary": null, "view": "work",
            "proposed_by": {"actor": "agent:a", "run": null}, "proposed_at": "2026-10-06T10:00:00Z",
            "canon_at": null, "state": "open", "landing": null, "changes": [], "blocks": 0,
            "conflicts": [], "blocking": [],
            "versions": [{"ref": "P-111111111111@2", "id": h('2'), "latest": true, "landed": false}],
            "removal_card": {"ref": "P-111111111111@2", "card": card, "card_hash": h('9'), "removal": []}
        });
        let head = "proposal P-111111111111@2 (version 222222222222): Pins\n\
                    view work, proposed by agent:a at 2026-10-06T10:00:00Z\n";
        let open = format!(
            "{head}state: open\n\
             changes: 0 (0 blocks)\n\
             versions: P-111111111111@2 (latest)\n\
             \n{card}\n\n\
             card hash: {}\n\
             approve with: rdy approve P-111111111111@2\n",
            h('9')
        );
        assert_eq!(proposal_text(&v), open);
        // a withdrawn version keeps its card and offers no approve command
        v["state"] = json!("withdrawn");
        v["blocking"] = json!(["withdrawn (view work closed)"]);
        let t = proposal_text(&v);
        assert!(
            t.ends_with(&format!(
                "\n{card}\n\ncard hash: {}\nblocking: withdrawn (view work closed)\n",
                h('9')
            )),
            "{t}"
        );
        // the card could not be read: the error row stands in its place
        v["state"] = json!("open");
        v["blocking"] = json!([]);
        v.as_object_mut().unwrap().remove("removal_card");
        v["removal_card_error"] = json!({
            "code": "E_REDACTION_BUDGET",
            "message": "The output budget is too small for the redaction warning.",
            "fix": "raise max_tokens"
        });
        assert_eq!(
            proposal_text(&v),
            format!(
                "{head}state: open\n\
                 changes: 0 (0 blocks)\n\
                 versions: P-111111111111@2 (latest)\n\
                 removal card: not read (E_REDACTION_BUDGET: The output budget is too small for the redaction warning.) (fix: raise max_tokens)\n\
                 approve with: rdy approve P-111111111111@2\n"
            )
        );
    }

    /// The JSON a show serialized to before rdx13 (plan amendment, rulings task-13-round1 and
    /// task-13-round2): its
    /// fields in order, each as serde writes it, with the three skipped when empty.
    fn before_rdx13(s: &ProposalShow) -> String {
        let mut parts: Vec<(&str, String)> = vec![];
        let mut put = |k: &'static str, v: String| parts.push((k, v));
        fn j<T: Serialize>(v: &T) -> String {
            serde_json::to_string(v).unwrap()
        }
        put("ref", j(&s.r#ref));
        put("id", j(&s.id));
        put("stable", j(&s.stable));
        put("version", j(&s.version));
        put("title", j(&s.title));
        put("summary", j(&s.summary));
        put("view", j(&s.view));
        put("proposed_by", j(&s.proposed_by));
        put("proposed_at", j(&s.proposed_at));
        put("canon_at", j(&s.canon_at));
        put("state", j(&s.state));
        put("landing", j(&s.landing));
        put("changes", j(&s.changes));
        put("blocks", j(&s.blocks));
        put("conflicts", j(&s.conflicts));
        put("blocking", j(&s.blocking));
        put("versions", j(&s.versions));
        if !s.advised.is_empty() {
            put("advised", j(&s.advised));
        }
        if s.advised_error.is_some() {
            put("advised_error", j(&s.advised_error));
        }
        if s.advised_digest.is_some() {
            put("advised_digest", j(&s.advised_digest));
        }
        let body: Vec<String> = parts
            .iter()
            .map(|(k, v)| format!("{}:{v}", serde_json::to_string(k).unwrap()))
            .collect();
        format!("{{{}}}", body.join(","))
    }

    /// rdx13 plan amendment (rulings task-13-round1 and task-13-round2): `removal_card` and
    /// `removal_card_error` are
    /// additive. A show without them serializes byte for byte as before; a show with one adds
    /// only that key, last, and every other key keeps its bytes and its place.
    #[test]
    fn rdx13_a_show_without_removal_serializes_as_before() {
        let h = |c: char| c.to_string().repeat(64);
        let full = ProposalShow {
            r#ref: "P-111111111111@2".into(),
            id: h('2'),
            stable: h('1'),
            version: 2,
            title: "Pins".into(),
            summary: Some("Pins record content.".into()),
            view: "work".into(),
            proposed_by: RelayedByRow {
                actor: "agent:a".into(),
                run: Some("r1".into()),
            },
            proposed_at: "2026-10-06T10:00:00Z".into(),
            canon_at: Some(h('3')),
            state: "open".into(),
            landing: None,
            changes: vec![ProposalChangeRow {
                change: h('4'),
                message: "pin it".into(),
                blocks: 2,
            }],
            blocks: 2,
            conflicts: vec![],
            blocking: vec!["1 blocking tension(s)".into()],
            versions: vec![ProposalVersionRow {
                r#ref: "P-111111111111@1".into(),
                id: h('1'),
                latest: false,
                landed: false,
            }],
            advised: vec![AdvisedRow {
                tension: "tn-000000000001".into(),
                reviewer: "agent:r".into(),
                advice: "block".into(),
                citations: vec![],
                accepted: false,
            }],
            advised_error: Some("E_IO: busy".into()),
            advised_digest: Some(h('5')),
            removal_card: None,
            removal_card_error: None,
        };
        let bare = ProposalShow {
            summary: None,
            canon_at: None,
            advised: vec![],
            advised_error: None,
            advised_digest: None,
            ..full.clone()
        };
        for s in [&full, &bare] {
            let now = serde_json::to_string(s).unwrap();
            assert_eq!(now, before_rdx13(s));
            assert!(!now.contains("removal_card"), "{now}");
            let card = RemovalCardShow {
                r#ref: s.r#ref.clone(),
                card: "the card".into(),
                card_hash: h('6'),
                removal: vec![],
            };
            let with_card = ProposalShow {
                removal_card: Some(card.clone()),
                ..s.clone()
            };
            let before = before_rdx13(s);
            assert_eq!(
                serde_json::to_string(&with_card).unwrap(),
                format!(
                    "{},\"removal_card\":{}}}",
                    &before[..before.len() - 1],
                    serde_json::to_string(&card).unwrap()
                )
            );
            let row = ErrorRow {
                code: "E_REDACTION_BUDGET".into(),
                message: "The output budget is too small for the redaction warning.".into(),
                fix: "Increase max_tokens, or propose fewer redaction requests at once.".into(),
            };
            let with_error = ProposalShow {
                removal_card_error: Some(row.clone()),
                ..s.clone()
            };
            assert_eq!(
                serde_json::to_string(&with_error).unwrap(),
                format!(
                    "{},\"removal_card_error\":{}}}",
                    &before[..before.len() - 1],
                    serde_json::to_string(&row).unwrap()
                )
            );
        }
    }

    /// rdx13 ruling task-13-round2: the wire shapes, key by key and in order. `removal_card` is
    /// `{ref, card, card_hash, removal}`, where each `removal` entry is Task 8's `RemovalCard`
    /// with its nine fields unchanged; `removal_card_error` is `{code, message, fix}`.
    #[test]
    fn rdx13_the_removal_card_fields_have_the_ruled_shapes() {
        let h = |c: char| c.to_string().repeat(64);
        let card = RemovalCardShow {
            r#ref: "P-111111111111@1".into(),
            card: "the card".into(),
            card_hash: h('6'),
            removal: vec![RemovalCard {
                request: h('7').parse().unwrap(),
                report: h('8').parse().unwrap(),
                versions: 1,
                blobs: 2,
                shared_blobs: 3,
                payloads: 4,
                bytes: 5,
                reason_class: rdy_model::redaction::ReasonClass::Secret,
                reason: "a leaked code".into(),
            }],
        };
        assert_eq!(
            serde_json::to_string(&card).unwrap(),
            format!(
                "{{\"ref\":\"P-111111111111@1\",\"card\":\"the card\",\"card_hash\":\"{}\",\
                 \"removal\":[{{\"request\":\"{}\",\"report\":\"{}\",\"versions\":1,\"blobs\":2,\
                 \"shared_blobs\":3,\"payloads\":4,\"bytes\":5,\"reason_class\":\"secret\",\
                 \"reason\":\"a leaked code\"}}]}}",
                h('6'),
                h('7'),
                h('8')
            )
        );
        let row = ErrorRow {
            code: "E_REDACTION_BUDGET".into(),
            message: "m".into(),
            fix: "f".into(),
        };
        assert_eq!(
            serde_json::to_string(&row).unwrap(),
            "{\"code\":\"E_REDACTION_BUDGET\",\"message\":\"m\",\"fix\":\"f\"}"
        );
    }
}
