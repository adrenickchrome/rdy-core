//! Spec §15.3 invariant 6, the landing audit: no landing put an open blocking tension into
//! canon, landed over an unresolved conflict, or chose no member of the alternative set its
//! changes are bound to. The audit re-checks the gate's rules from public data on the state the
//! landing read (D-CRTJG, D-84QSN, D-VA8DA, D-WMCFW); it never calls the gate. The conflict rule
//! is gconf1's: the landing's own changes merged onto the view's effective base, on the blocks
//! they write, never the view's current conflict set.
use crate::invariants::Violation;
use rdy_engine::tensions::{SideRef, TensionSeverity, TensionStatus, tension_label};
use rdy_engine::{Rdy, ReadState};
use rdy_model::{BlockId, Change, Hash, LandingRecord};
use rdy_store::merge::{Conflict, MergeChange, merge_on_blocks};
use std::collections::{BTreeMap, BTreeSet, HashMap};

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
    // conflict_landed: gconf1 re-checked from public data: the landing's own changes merged
    // onto the view's effective base, on the blocks they write (never the view's current
    // conflict set: a subset may leave a conflict out, and a version that predates the view's
    // resolution still carries it)
    out.extend(conflict_rule(
        before,
        view,
        before.effective_base(view),
        &read,
    ));
    // alt_unchosen: a change bound to a set whose member the landing did not choose, unless an
    // earlier landing of canon's chain chose that member
    match before.reader().landings() {
        Ok(chain) => out.extend(unchosen_rule(view, landed, &read, &chosen_members(&chain))),
        Err(e) => out.push(viol(
            "alt_unchosen",
            None,
            format!("landing of {view}: cannot read canon's landings: {e}"),
        )),
    }
    out
}

/// The member each alternative set was chosen for, by the set's stable hash, as the landing
/// records of `chain` (canon's chain, in any order) state it. Read from the records alone, never
/// from the gate.
pub fn chosen_members(chain: &[(Hash, LandingRecord)]) -> BTreeMap<Hash, String> {
    chain
        .iter()
        .filter_map(|(_, rec)| rec.chosen.as_ref())
        .map(|c| (c.set, c.member.clone()))
        .collect()
}

/// Rule `alt_unchosen` alone: each change of `landed` linked to a set (`links.alternative`) needs
/// the landing to choose that set, or the set to be chosen already for the member the change is
/// linked to. `settled` is `chosen_members` of the landings before this one. Once a set is chosen,
/// only its losers stay bound (FW-AL1; gate rule 2 of `linked_alts`, ruling task-5.1).
pub fn alt_unchosen(
    st: &ReadState,
    landed: &Landed,
    settled: &BTreeMap<Hash, String>,
) -> Vec<Violation> {
    let view = &landed.view;
    let reader = st.reader();
    let mut out = vec![];
    let mut read = vec![];
    for c in &landed.changes {
        match reader.change(c) {
            Ok(ch) => read.push((*c, ch)),
            Err(e) => out.push(viol(
                "alt_unchosen",
                None,
                format!("landing of {view}: cannot read change {}: {e}", c.short()),
            )),
        }
    }
    out.extend(unchosen_rule(view, landed, &read, settled));
    out
}

fn unchosen_rule(
    view: &str,
    landed: &Landed,
    changes: &[(Hash, Change)],
    settled: &BTreeMap<Hash, String>,
) -> Vec<Violation> {
    let mut out = vec![];
    for (c, ch) in changes {
        let Some(choice) = &ch.links.alternative else {
            continue;
        };
        if landed.alternative == Some(choice.set) {
            continue;
        }
        let detail = match settled.get(&choice.set) {
            Some(member) if *member == choice.member => continue,
            Some(member) => format!(
                "landing of {view}: change {} is bound to member {} of set {}, which chose {member}, and the landing chose no member of it",
                c.short(),
                choice.member,
                choice.set.short()
            ),
            None => format!(
                "landing of {view}: change {} is bound to set {}, and the landing chose no member of it",
                c.short(),
                choice.set.short()
            ),
        };
        out.push(viol("alt_unchosen", None, detail));
    }
    out
}

/// Rule `conflict_landed` alone, on `base`: the effective base view `landed.view` read on when
/// it landed. A state after the landing no longer shows that base, so a caller that read it
/// before the landing passes it here (the swarm reads it through the relay, sg5).
pub fn conflicts_at(st: &ReadState, landed: &Landed, base: Option<Hash>) -> Vec<Violation> {
    let view = &landed.view;
    let reader = st.reader();
    let mut out = vec![];
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
    out.extend(conflict_rule(st, view, base, &read));
    out
}

/// The `conflict_landed` violations of `changes` (one landing's changes, proposed from view
/// `view`) merged onto `base`.
fn conflict_rule(
    st: &ReadState,
    view: &str,
    base: Option<Hash>,
    changes: &[(Hash, Change)],
) -> Vec<Violation> {
    match carried_conflicts(st, base, changes) {
        Ok(cs) => cs
            .iter()
            .map(|c| {
                viol(
                    "conflict_landed",
                    Some(&c.block),
                    format!(
                        "landing of {view}: block {} had an unresolved conflict",
                        c.block
                    ),
                )
            })
            .collect(),
        Err(e) => vec![viol(
            "conflict_landed",
            None,
            format!("landing of {view}: cannot merge its changes: {e}"),
        )],
    }
}

/// The conflicts `changes` (one landing's changes) carry on the blocks they write: their merge
/// onto `base` (the proposing view's effective base), restricted to those blocks.
/// The producer of each touched block's base version is read up front, so a damaged index
/// entry is an error, never "no producer".
fn carried_conflicts(
    st: &ReadState,
    base: Option<Hash>,
    changes: &[(Hash, Change)],
) -> Result<Vec<Conflict>, String> {
    let reader = st.reader();
    let state = reader
        .state_of(base)
        .map_err(|e| format!("cannot read the view's base: {e}"))?;
    let touched: BTreeSet<BlockId> = changes
        .iter()
        .flat_map(|(_, ch)| ch.ops.iter().map(|op| op.block_id().clone()))
        .collect();
    let input: Vec<MergeChange<'_>> = changes
        .iter()
        .map(|(id, ch)| MergeChange {
            id: *id,
            deps: &ch.deps,
            ops: &ch.ops,
            is_replay: ch.links.replayed_from.is_some(),
        })
        .collect();
    let deleters = reader
        .merge_deleters(base.as_ref(), &state, &touched)
        .map_err(|e| format!("cannot read the deleters: {e}"))?;
    let mut producers: HashMap<Hash, Option<Hash>> = HashMap::new();
    for v in touched.iter().filter_map(|b| state.blocks.get(b)) {
        if !producers.contains_key(v) {
            let p = reader
                .producer(v)
                .map_err(|e| format!("cannot read the producer of {}: {e}", v.short()))?;
            producers.insert(*v, p);
        }
    }
    let order_only = reader.order_only_pairs(&state, changes.iter().flat_map(|(_, ch)| &ch.ops));
    Ok(merge_on_blocks(
        &state,
        &input,
        &|v| producers.get(v).copied().flatten(),
        &|b| deleters.get(b).copied(),
        &|v, op| order_only.holds(v, op),
        &touched,
    )
    .conflicts)
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
