//! O10, O11 (D-TF78H, D-ZWP7Q): the policy role a queue item needs, and who holds it. A policy
//! role is a named group of people in the authority policy, not a session role. These are the only
//! role rules: routing, the approval check and the verdict's `role` call them.
use super::owners::owner_entry;
use crate::error::{E_PERMISSION, RdyError};
use crate::pending::ItemKind;
use crate::{Materialized, ReadState};
use rdy_model::BlockId;
use rdy_schema::policy::{PolicyFields, When};

/// What `require` entries match on (O10). `blocks` are the blocks the item covers: for an
/// `approve` or `confirm` item, the blocks its version writes; a `defer` item, the subjects of the
/// deferred items; a `reaffirm` item, its member decisions; an `answer` item, its question block;
/// a `resolve` item, the tension's blocks. `one_way`: the item concerns a decision declared
/// `one_way`, written or re-affirmed. `decision`: its version writes a decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemFacts {
    pub kind: ItemKind,
    pub blocks: Vec<BlockId>,
    pub one_way: bool,
    pub decision: bool,
}

/// The people an item needs: a role of the policy, or the owners of an owner entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequiredRole {
    Role { name: String, members: Vec<String> },
    Owners { path: String, members: Vec<String> },
}

impl RequiredRole {
    /// The role's name, or `owners of <path>`.
    pub fn label(&self) -> String {
        match self {
            RequiredRole::Role { name, .. } => name.clone(),
            RequiredRole::Owners { path, .. } => format!("owners of {path}"),
        }
    }

    fn members(&self) -> &[String] {
        match self {
            RequiredRole::Role { members, .. } | RequiredRole::Owners { members, .. } => members,
        }
    }
}

/// The owners of the most specific entry of the first covered block, in block id order, that has
/// one.
fn owners(m: &Materialized, p: &PolicyFields, blocks: &[BlockId]) -> Option<RequiredRole> {
    let mut sorted: Vec<&BlockId> = blocks.iter().collect();
    sorted.sort();
    sorted.dedup();
    sorted.into_iter().find_map(|b| {
        owner_entry(m, p, b).map(|e| RequiredRole::Owners {
            path: e.path.clone(),
            members: e.owners.clone(),
        })
    })
}

/// Whether a block `item` covers is in area `area` of `m`. A block `m` does not hold is in no
/// area.
fn in_area(st: &ReadState, m: &Materialized, item: &ItemFacts, area: &str) -> bool {
    item.blocks.iter().any(|id| {
        m.get(id)
            .is_some_and(|b| st.areas_in(m, b).iter().any(|a| a == area))
    })
}

/// O10: the first matching `require` entry's people; None when none matches.
///
/// Entries match in list order: `one_way` when the item concerns a `one_way` decision;
/// `decision` when its version writes a decision; `area:<a>` when a covered block is in area
/// `<a>`; `deferral` for a `defer` item; `reaffirm` for a `reaffirm` item; `owner` when a covered
/// block has an owner entry; `any` always. `p` is a valid policy's fields, so every named role
/// exists; an entry naming no role of `roles` names a role with no member.
pub fn required_role(
    st: &ReadState,
    m: &Materialized,
    p: &PolicyFields,
    item: &ItemFacts,
) -> Option<RequiredRole> {
    for entry in &p.require {
        let matched = match &entry.when {
            When::OneWay => item.one_way,
            When::Decision => item.decision,
            When::Area(area) => in_area(st, m, item, area),
            When::Deferral => item.kind == ItemKind::Defer,
            When::Reaffirm => item.kind == ItemKind::Reaffirm,
            When::Owner => {
                if let Some(found) = owners(m, p, &item.blocks) {
                    return Some(found);
                }
                false
            }
            When::Any => true,
        };
        if matched {
            let name = entry.role.clone().unwrap_or_default();
            let members = p.roles.get(&name).cloned().unwrap_or_default();
            return Some(RequiredRole::Role { name, members });
        }
    }
    None
}

/// O11: whether `person` holds `role`. A `human:<name>` member holds that actor only. A
/// `team:<name>` member holds every person locally: core cannot know a team's members, and the
/// hub checks them (D-TF78H).
pub fn holds(role: &RequiredRole, person: &str) -> bool {
    role.members()
        .iter()
        .any(|m| m.starts_with("team:") || m.as_str() == person)
}

/// D-TF78H: the refusal of an approval or an answer to item `r` by `person`, who does not hold
/// `role`.
pub fn outside_role(person: &str, role: &RequiredRole, r: &str) -> RdyError {
    let label = role.label();
    RdyError::new(
        E_PERMISSION,
        format!("{person} is not in the policy role {label} for {r}"),
        format!("ask a person in the policy role {label}, or relay their answer"),
    )
}
