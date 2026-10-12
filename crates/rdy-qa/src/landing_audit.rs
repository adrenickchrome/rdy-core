//! Spec §15.3 invariant 6, the landing audit: no landing put an open blocking tension into
//! canon, landed over an unresolved conflict, or chose no member of the alternative set its
//! changes are bound to. The audit re-checks the gate's rules from public data on the state the
//! landing read (D-CRTJG, D-84QSN, D-VA8DA, D-WMCFW); it never calls the gate.
use crate::invariants::Violation;
use rdy_engine::tensions::{SideRef, TensionSeverity, TensionStatus, tension_label};
use rdy_engine::{Rdy, ReadState, Scope};
use rdy_model::{BlockId, Hash};
use std::collections::BTreeSet;

/// One landing as the audit reads it: the view it was proposed from, the changes it landed and
/// the stable hash of the alternative set it chose (`LandingRecord.chosen`'s `set`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landed {
    pub view: String,
    pub changes: Vec<Hash>,
    pub alternative: Option<Hash>,
}

fn viol(rule: &'static str, block: Option<&BlockId>, detail: String) -> Violation {
    Violation {
        rule,
        block: block.cloned(),
        detail,
    }
}

/// The violations of landing `landed` on the state `before`: rules `blocking_landed`,
/// `conflict_landed` and `alt_unchosen`. A state that cannot be read is a violation of the rule
/// that needed it.
pub fn audit(before: &ReadState, landed: &Landed) -> Vec<Violation> {
    let mut out = vec![];
    let view = &landed.view;
    let mine: BTreeSet<&Hash> = landed.changes.iter().collect();
    // blocking_landed: open, blocking, and every side canon or one of the landing's changes
    match before.landing_tensions(view, &landed.changes) {
        Ok(ts) => {
            for t in ts {
                let all_in = t.sides.iter().all(|s| match s {
                    SideRef::Canon { .. } => true,
                    SideRef::Change(h) => mine.contains(h),
                    SideRef::Intent { .. } => false,
                });
                if t.severity == TensionSeverity::Blocking
                    && before.tension_status(&t) == TensionStatus::Open
                    && all_in
                {
                    out.push(viol(
                        "blocking_landed",
                        None,
                        format!(
                            "landing of {view}: tension {} ({}) was open and blocking with every side in canon or the proposal",
                            tension_label(&t.id),
                            t.kind.name()
                        ),
                    ));
                }
            }
        }
        Err(e) => out.push(viol(
            "blocking_landed",
            None,
            format!("landing of {view}: cannot read its tensions: {e}"),
        )),
    }
    // the changes, read once for the two rules that follow
    let reader = before.reader();
    let mut read = vec![];
    for c in &landed.changes {
        match reader.change(c) {
            Ok(ch) => read.push((*c, ch)),
            Err(e) => out.push(viol(
                "conflict_landed",
                None,
                format!("landing of {view}: cannot read change {}: {e}", c.short()),
            )),
        }
    }
    // conflict_landed: an unresolved conflict of the view on a block one of the changes writes
    let written: BTreeSet<&BlockId> = read
        .iter()
        .flat_map(|(_, ch)| ch.ops.iter().map(|op| op.block_id()))
        .collect();
    match before.conflicts(&Scope::View(view.clone())) {
        Ok(cs) => {
            for c in cs.iter().filter(|c| written.contains(&c.block)) {
                out.push(viol(
                    "conflict_landed",
                    Some(&c.block),
                    format!(
                        "landing of {view}: block {} had an unresolved conflict",
                        c.block
                    ),
                ));
            }
        }
        Err(e) => out.push(viol(
            "conflict_landed",
            None,
            format!("landing of {view}: cannot read its conflicts: {e}"),
        )),
    }
    // alt_unchosen: a change bound to a set whose member the landing did not choose
    for (c, ch) in &read {
        if let Some(choice) = &ch.links.alternative
            && landed.alternative != Some(choice.set)
        {
            out.push(viol(
                "alt_unchosen",
                None,
                format!(
                    "landing of {view}: change {} is bound to set {}, and the landing chose no member of it",
                    c.short(),
                    choice.set.short()
                ),
            ));
        }
    }
    out
}

/// The landing `landing` of `after`'s canon as the audit reads it: the changes and the chosen
/// set of its `LandingRecord`, and the view of its proposal version.
pub fn landed_of(after: &Rdy, landing: &Hash) -> Result<Landed, String> {
    let st = after.read();
    let reader = st.reader();
    let landings = reader
        .landings()
        .map_err(|e| format!("cannot read landings: {e}"))?;
    let rec = landings
        .into_iter()
        .find(|(h, _)| h == landing)
        .map(|(_, rec)| rec)
        .ok_or_else(|| format!("landing {} is not in canon's chain", landing.short()))?;
    let proposal = reader
        .proposal(&rec.proposal)
        .map_err(|e| format!("cannot read proposal {}: {e}", rec.proposal.short()))?;
    Ok(Landed {
        view: proposal.view,
        changes: rec.changes,
        alternative: rec.chosen.map(|c| c.set),
    })
}
