//! Tensions that need a person (D-WMCFW, D-562QR): two kinds of `resolve` item.
//!
//! # Advice (D-WMCFW)
//!
//! A ready batch whose version has advised tensions that no one accepted gets one `resolve`
//! item, ref `advice:P-x@n`. It stands beside the batch, after its exceptions, and the person
//! answers once for the batch: accept the advice or dismiss with a reason. Its `members` are one
//! line a tension, with the reviewer's first finding, and its one fact is the advised digest.
//!
//! # Blocking tensions (D-562QR)
//!
//! Each open blocking tension that involves the latest version of an open proposal gets one
//! `resolve` item on its own, ref the tension's label (`tn-<12 hex>`), `blocking: true`. A
//! blocking tension is dismissed by a reviewer-role or human session whose actor wrote none of
//! its sides, so an agent that wrote a side can never clear it alone. A relay with the person's
//! words counts as the person. Its options are `dismiss` and `later`: the person has no
//! "accept" for a tension that already blocks.
//!
//! A tension that blocks because the person accepted the advice is an item too: accepting makes
//! it block, it does not resolve or dismiss it. The advice card is gone after the accept; this
//! item stays while the tension is open.
//!
//! The list is closed. Two cases are no item:
//! - a tension that is dismissed (it is not open);
//! - a tension of a version whose proposal the person rejected.
//!
//! A version that is not ready is still searched: the person's dismissal may be what makes it
//! ready.
use super::item::{Fact, Item, ItemKind};
use super::queue::{answered, item, later, open_tensions, option};
use crate::ReadState;
use crate::error::RdyError;
use crate::gate::ProposalRef;
use crate::session::{now_ms, rfc3339};
use crate::tensions::{Tension, TensionSeverity, tension_label};
use rdy_model::{AnswerAct, Proposal};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The `resolve` items of the queue: the advice item of each batch in `batches` (the ready
/// `approve` items), then the item of each blocking tension. An advice item names its batch in
/// `batch`, and copies who raised the batch and when. `open` is the open proposals the queue read ([`crate::gate::open_proposals`]), shared so
/// the queue reads them once.
pub(crate) fn tension_items(
    st: &ReadState,
    open: &[(ProposalRef, Proposal)],
    batches: &[Item],
) -> Result<Vec<Item>, RdyError> {
    let now = now_ms();
    let mut out = vec![];
    for batch in batches {
        let Some((_, p)) = open
            .iter()
            .find(|(pref, _)| pref.to_string() == batch.r#ref)
        else {
            continue;
        };
        out.extend(advice_item(st, batch, p)?);
    }
    out.extend(blocking_items(st, open, now)?);
    Ok(out)
}

/// The `advice:P-x@n` item of `batch`, if its version has advised tensions no one accepted.
fn advice_item(st: &ReadState, batch: &Item, p: &Proposal) -> Result<Option<Item>, RdyError> {
    let advised = st.advised_of(&p.view, &p.changes)?;
    let mut open: Vec<_> = advised.iter().filter(|a| !a.open.record.blocking).collect();
    if open.is_empty() {
        return Ok(None);
    }
    open.sort_by_key(|a| a.tension.id);
    let r = &batch.r#ref;
    let n = open.len();
    let title = if n == 1 {
        "1 tension a reviewer advised to block".to_string()
    } else {
        format!("{n} tensions a reviewer advised to block")
    };
    let mut it = item(ItemKind::Resolve, &format!("advice:{r}"), &title);
    it.why = format!("the reviewers advise that these block {r}");
    it.members = open
        .iter()
        .map(|a| {
            let info = st.tension_info(&a.tension);
            let finding = info
                .judged
                .and_then(|j| j.findings.into_iter().next())
                .map_or(a.tension.summary.clone(), |f| f.note);
            format!("{}  {finding}", tension_label(&a.tension.id))
        })
        .collect();
    it.facts = vec![Fact {
        name: "Advised digest",
        value: crate::semantic::accept_advice::advised_digest(&advised).unwrap_or_default(),
    }];
    it.options = vec![
        option(
            "accept",
            "Accept the advice: they block",
            "The advised tensions block until a side changes.",
            Some("the reviewer advised it".to_string()),
        ),
        option(
            "dismiss",
            "Dismiss with a reason",
            "Dismisses each advised tension with your reason.",
            None,
        ),
        later(),
    ];
    it.batch = Some(r.clone());
    it.raised_by.clone_from(&batch.raised_by);
    it.raised_at.clone_from(&batch.raised_at);
    it.age.clone_from(&batch.age);
    Ok(Some(it))
}

/// One blocking tension and the versions it involves, in ref order.
struct Blocking {
    tension: Arc<Tension>,
    versions: Vec<String>,
    /// The version that raised it: the first by ref.
    raised: Proposal,
}

/// The `resolve` item of each open blocking tension of an open proposal's latest version, in
/// tension-id order.
fn blocking_items(
    st: &ReadState,
    open: &[(ProposalRef, Proposal)],
    now: i64,
) -> Result<Vec<Item>, RdyError> {
    let mut found: BTreeMap<rdy_model::Hash, Blocking> = BTreeMap::new();
    for (pref, p) in open {
        let r = pref.to_string();
        if answered(st, &r, AnswerAct::RejectProposal)? {
            continue;
        }
        for t in open_tensions(st, p)? {
            if t.severity != TensionSeverity::Blocking {
                continue;
            }
            found
                .entry(t.id)
                .or_insert_with(|| Blocking {
                    tension: Arc::clone(&t),
                    versions: vec![],
                    raised: p.clone(),
                })
                .versions
                .push(r.clone());
        }
    }
    Ok(found
        .into_values()
        .map(|b| blocking_item(&b, now))
        .collect())
}

fn blocking_item(b: &Blocking, now: i64) -> Item {
    let t = &b.tension;
    let versions = b.versions.join(", ");
    let mut it = item(ItemKind::Resolve, &tension_label(&t.id), &t.summary);
    it.blocking = true;
    it.why = format!(
        "a blocking tension involves {versions}; only a person or a reviewer can dismiss it"
    );
    it.facts = vec![
        Fact {
            name: "Kind",
            value: t.kind.name().to_string(),
        },
        Fact {
            name: "Involves",
            value: versions,
        },
        Fact {
            name: "Blocks",
            value: t
                .blocks()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        },
    ];
    it.options = vec![
        option(
            "dismiss",
            "Dismiss with a reason",
            "Dismisses the tension with your reason.",
            None,
        ),
        later(),
    ];
    let at = b.raised.provenance.at_ms;
    it.raised_by.clone_from(&b.raised.provenance.actor);
    it.raised_at = rfc3339(at);
    it.age = super::item::age(at, now);
    it
}
