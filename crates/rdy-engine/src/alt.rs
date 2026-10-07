//! alts1: alternative sets (plan M2d1 Task 7). A set groups two or more open member views that
//! answer one question; a view is a member of at most one open set. Create, add, remove and
//! abandon each write a new version of the set; abandon also closes every open member view in
//! the same batch. Changes written in a member view record the set (`write_linked`).
use crate::error::*;
use crate::session::rfc3339;
use crate::views::OwnerInfo;
use crate::{Compare, Delta, Rdy, ReadState, Scope, Session};
use rdy_model::{AltMember, AltSet, AltStatus, BlockId, Hash, Provenance};
use rdy_store::coord::abandoned_reason;
use serde::{Deserialize, Serialize};

pub use crate::coord_graph::alt_label;

/// The hint for an unknown or ambiguous set.
const LIST_SETS: &str = "list the sets with rdy alt ls (MCP: rdy_alternatives list)";

/// A member as create and add take it: a label and the view that holds the alternative.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct AltMemberInput {
    pub label: String,
    pub view: String,
}

/// A member as reads show it, with its view's owner, state and included change count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AltMemberInfo {
    pub label: String,
    pub view: String,
    pub owner: Option<OwnerInfo>,
    pub closed: bool,
    pub changes: usize,
}

/// The member a landing chose, and that landing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChosenInfo {
    pub member: String,
    pub landing: String,
}

/// An alternative set at its latest version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AltSetInfo {
    /// `as-<12 hex>`.
    pub id: String,
    /// The stable id (the first version's hash).
    pub set: String,
    /// The latest version.
    pub version: String,
    pub prev: Option<String>,
    pub title: String,
    pub question: Option<String>,
    pub members: Vec<AltMemberInfo>,
    /// `open`, `chosen` or `abandoned`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chosen: Option<ChosenInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Who wrote the latest version, and when (RFC 3339 UTC).
    pub changed_by: OwnerInfo,
    pub changed_at: String,
    /// Who wrote the first version.
    pub created_by: OwnerInfo,
}

/// One member of a compare: its view against canon.
#[derive(Clone, Debug, Serialize)]
pub struct AltCompareRow {
    pub label: String,
    pub view: String,
    pub compare: Compare,
}

/// Each member of a set against canon, in member order.
#[derive(Clone, Debug, Serialize)]
pub struct AltCompare {
    pub set: AltSetInfo,
    pub members: Vec<AltCompareRow>,
}

/// A proposal's pick: the set and the chosen member.
#[derive(Clone, Debug, Deserialize)]
pub struct AltPick {
    pub set: String,
    pub chosen: String,
}

fn by_actor(p: &Provenance) -> OwnerInfo {
    OwnerInfo {
        actor: p.actor.clone(),
        run: p.run.clone(),
    }
}

fn status_name(s: &AltStatus) -> &'static str {
    match s {
        AltStatus::Open => "open",
        AltStatus::Chosen { .. } => "chosen",
        AltStatus::Abandoned { .. } => "abandoned",
    }
}

fn bad(message: String, fix: &str) -> RdyError {
    RdyError::new(E_BAD_INPUT, message, fix)
}

fn need_writer(s: &Session) -> Result<(), RdyError> {
    if s.role.can_write() {
        return Ok(());
    }
    Err(RdyError::new(
        E_PERMISSION,
        format!("role {:?} cannot change alternative sets", s.role),
        "start the session as a writer",
    ))
}

fn need_label(m: &AltMemberInput) -> Result<(), RdyError> {
    if m.label.trim().is_empty() {
        return Err(bad(
            format!("member {} has no label", m.view),
            "name each member view with a label",
        ));
    }
    Ok(())
}

/// Each label and each view at most once among `members`.
fn distinct(members: &[AltMember]) -> Result<(), RdyError> {
    for (i, m) in members.iter().enumerate() {
        let earlier = &members[..i];
        if earlier.iter().any(|e| e.label == m.label) {
            return Err(bad(
                format!("label {} is used twice", m.label),
                "give each member its own label",
            ));
        }
        if earlier.iter().any(|e| e.view == m.view) {
            return Err(bad(
                format!("view {} is used twice", m.view),
                "name each member view once",
            ));
        }
    }
    Ok(())
}

fn need_open(stable: &Hash, set: &AltSet) -> Result<(), RdyError> {
    match &set.status {
        AltStatus::Open => Ok(()),
        s => Err(bad(
            format!("{} is {}", alt_label(stable), status_name(s)),
            "only an open set changes; list the open sets with rdy alt ls (MCP: rdy_alternatives list)",
        )),
    }
}

impl ReadState {
    /// Resolves a set's id: `as-<hex>` or `<hex>`, the full stable id or a unique prefix of at
    /// least 6 hex digits. Returns the stable id, the latest version and that version.
    pub fn resolve_alt(&self, text: &str) -> Result<(Hash, Hash, AltSet), RdyError> {
        let t = text.trim();
        let hex = t.strip_prefix("as-").unwrap_or(t).to_ascii_lowercase();
        let none = || RdyError::new(E_NOT_FOUND, format!("no alternative set {t}"), LIST_SETS);
        if !(6..=64).contains(&hex.len()) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(none());
        }
        let found: Vec<(&Hash, &Hash)> = self
            .coord_heads
            .alt_sets
            .iter()
            .filter(|(stable, _)| stable.to_string().starts_with(&hex))
            .collect();
        match found.as_slice() {
            [] => Err(none()),
            [(stable, version)] => Ok((**stable, **version, self.reader().alt_set(version)?)),
            many => Err(RdyError::new(
                E_NOT_FOUND,
                format!("{t} names {} alternative sets", many.len()),
                format!(
                    "use more of the id: {}",
                    many.iter()
                        .map(|(s, _)| s.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        }
    }

    /// The open set view `view` is a member of, if any: its stable id, latest version and that
    /// version.
    pub fn alt_of_view(&self, view: &str) -> Result<Option<(Hash, Hash, AltSet)>, RdyError> {
        for (stable, version) in &self.coord_heads.alt_sets {
            let set = self.reader().alt_set(version)?;
            if set.status == AltStatus::Open && set.members.iter().any(|m| m.view == view) {
                return Ok(Some((*stable, *version, set)));
            }
        }
        Ok(None)
    }

    /// The open sets, or every set when `all`, by title then id.
    pub fn alt_sets(&self, all: bool) -> Result<Vec<AltSetInfo>, RdyError> {
        let mut out = vec![];
        for (stable, version) in &self.coord_heads.alt_sets {
            let set = self.reader().alt_set(version)?;
            if all || set.status == AltStatus::Open {
                out.push(self.alt_info(*stable, *version, &set)?);
            }
        }
        out.sort_by(|a, b| (&a.title, &a.id).cmp(&(&b.title, &b.id)));
        Ok(out)
    }

    /// Set `set`'s members, each against canon.
    pub fn alt_compare(&self, set: &str) -> Result<AltCompare, RdyError> {
        let (stable, version, s) = self.resolve_alt(set)?;
        let members = s
            .members
            .iter()
            .map(|m| {
                Ok(AltCompareRow {
                    label: m.label.clone(),
                    view: m.view.clone(),
                    compare: self.compare(&Scope::View(m.view.clone()), &Scope::Canon)?,
                })
            })
            .collect::<Result<Vec<_>, RdyError>>()?;
        Ok(AltCompare {
            set: self.alt_info(stable, version, &s)?,
            members,
        })
    }

    /// Set version `set` (the latest, `version`, of set `stable`) as reads show it.
    pub(crate) fn alt_info(
        &self,
        stable: Hash,
        version: Hash,
        set: &AltSet,
    ) -> Result<AltSetInfo, RdyError> {
        let created_by = if set.prev.is_none() {
            by_actor(&set.provenance)
        } else {
            by_actor(&self.reader().alt_set(&stable)?.provenance)
        };
        let members = set
            .members
            .iter()
            .map(|m| {
                let head = self.view_head(&m.view);
                AltMemberInfo {
                    label: m.label.clone(),
                    view: m.view.clone(),
                    owner: head.and_then(|h| h.owner.as_ref()).map(OwnerInfo::from),
                    closed: head.is_none_or(|h| h.closed),
                    changes: self.included(&m.view).len(),
                }
            })
            .collect();
        let (chosen, reason) = match &set.status {
            AltStatus::Open => (None, None),
            AltStatus::Chosen { member, landing } => (
                Some(ChosenInfo {
                    member: member.clone(),
                    landing: landing.to_string(),
                }),
                None,
            ),
            AltStatus::Abandoned { reason } => (None, Some(reason.clone())),
        };
        Ok(AltSetInfo {
            id: alt_label(&stable),
            set: stable.to_string(),
            version: version.to_string(),
            prev: set.prev.map(|p| p.to_string()),
            title: set.title.clone(),
            question: set.question.as_ref().map(BlockId::to_string),
            members,
            status: status_name(&set.status).to_string(),
            chosen,
            reason,
            changed_by: by_actor(&set.provenance),
            changed_at: rfc3339(set.provenance.at_ms),
            created_by,
        })
    }

    /// Add, remove and abandon are the creator's actor's (any run) or a human's.
    fn may_change_alt(&self, s: &Session, stable: &Hash) -> Result<(), RdyError> {
        if s.is_human() {
            return Ok(());
        }
        let creator = self.reader().alt_set(stable)?.provenance.actor;
        if creator == s.actor {
            return Ok(());
        }
        Err(RdyError::new(
            E_PERMISSION,
            format!("{} was created by {creator}", alt_label(stable)),
            "ask its creator or a human",
        ))
    }

    /// View `view` may join a set: it is an open view in no open set other than `own`.
    fn may_join(&self, view: &str, own: Option<&Hash>) -> Result<(), RdyError> {
        let Some(head) = self.view_head(view) else {
            return Err(RdyError::new(
                E_ALT_MEMBER,
                format!("member {view} is not a view"),
                "create or fork the view first (rdy_view)",
            ));
        };
        if head.closed {
            return Err(RdyError::new(
                E_ALT_MEMBER,
                format!("member view {view} is closed"),
                "reopen it (undo the close) or fork it",
            ));
        }
        if let Some((other, _, _)) = self.alt_of_view(view)?
            && Some(&other) != own
        {
            return Err(RdyError::new(
                E_ALT_MEMBER,
                format!("view {view} is already a member of {}", alt_label(&other)),
                "remove it there first, or fork it",
            ));
        }
        Ok(())
    }

    /// The question block `text` names: resolved in the session's view, or in the in-flight
    /// state (canon and every open view) when the session has no view.
    fn alt_question(&self, s: &Session, text: &str) -> Result<BlockId, RdyError> {
        let own = s.default_view();
        let scope = if self.has_view(&own) {
            Scope::View(own)
        } else {
            Scope::Inflight
        };
        let id = self.resolve_id(&scope, text)?;
        let m = self.materialized(&scope)?;
        let Some(b) = m.get(&id) else {
            return Err(RdyError::new(
                E_UNKNOWN_BLOCK,
                format!("no block {id}"),
                "check the id with rdy_read",
            ));
        };
        if b.block_type != "question" {
            return Err(bad(
                format!("{id} is a {}, not a question", b.block_type),
                "pass the id of a question block, or leave question out",
            ));
        }
        Ok(id)
    }
}

impl Rdy {
    /// Creates an alternative set of `members` (two or more open views, each in no other open
    /// set), titled `title`, optionally answering the question block `question`.
    pub fn alt_create(
        &mut self,
        s: &Session,
        title: &str,
        question: Option<&str>,
        members: &[AltMemberInput],
    ) -> Result<AltSetInfo, RdyError> {
        need_writer(s)?;
        if title.trim().is_empty() {
            return Err(bad(
                "an alternative set needs a title".to_string(),
                "name the question the members answer in a few words",
            ));
        }
        if members.len() < 2 {
            return Err(bad(
                "an alternative set needs at least two members".to_string(),
                "name each member view with a label",
            ));
        }
        members.iter().try_for_each(need_label)?;
        let members: Vec<AltMember> = members
            .iter()
            .map(|m| AltMember {
                label: m.label.clone(),
                view: m.view.clone(),
            })
            .collect();
        distinct(&members)?;
        let st = self.read();
        for m in &members {
            st.may_join(&m.view, None)?;
        }
        let question = question.map(|q| st.alt_question(s, q)).transpose()?;
        drop(st);
        let next = AltSet {
            stable: None,
            prev: None,
            title: title.to_string(),
            question,
            members,
            status: AltStatus::Open,
            provenance: s.provenance(),
        };
        let version = self.store.put_alt_set(&next)?;
        self.publish(Delta::Coord { closed: vec![] })?;
        self.read().alt_info(version, version, &next)
    }

    /// Adds member `m` to open set `set`.
    pub fn alt_add(
        &mut self,
        s: &Session,
        set: &str,
        m: &AltMemberInput,
    ) -> Result<AltSetInfo, RdyError> {
        need_writer(s)?;
        need_label(m)?;
        let st = self.read();
        let (stable, version, cur) = st.resolve_alt(set)?;
        need_open(&stable, &cur)?;
        st.may_change_alt(s, &stable)?;
        let mut members = cur.members.clone();
        members.push(AltMember {
            label: m.label.clone(),
            view: m.view.clone(),
        });
        distinct(&members)?;
        st.may_join(&m.view, Some(&stable))?;
        drop(st);
        self.next_alt_version(s, stable, version, cur, members)
    }

    /// Removes member `member` (a label, else a view) from open set `set`. A set keeps at least
    /// two members: abandon it instead.
    pub fn alt_remove(
        &mut self,
        s: &Session,
        set: &str,
        member: &str,
    ) -> Result<AltSetInfo, RdyError> {
        need_writer(s)?;
        let st = self.read();
        let (stable, version, cur) = st.resolve_alt(set)?;
        need_open(&stable, &cur)?;
        st.may_change_alt(s, &stable)?;
        drop(st);
        let at = cur
            .members
            .iter()
            .position(|m| m.label == member)
            .or_else(|| cur.members.iter().position(|m| m.view == member))
            .ok_or_else(|| {
                RdyError::new(
                    E_NOT_FOUND,
                    format!("{} has no member {member}", alt_label(&stable)),
                    "name a member by its label; list them with rdy alt ls (MCP: rdy_alternatives list)",
                )
            })?;
        if cur.members.len() <= 2 {
            return Err(bad(
                format!(
                    "removing {member} would leave {} with one member",
                    alt_label(&stable)
                ),
                "abandon the set instead (abandon closes every member view)",
            ));
        }
        let mut members = cur.members.clone();
        members.remove(at);
        self.next_alt_version(s, stable, version, cur, members)
    }

    /// Abandons open set `set` for `reason` and closes every open member view in the same
    /// batch. Returns the set and the views it closed, in member order.
    pub fn alt_abandon(
        &mut self,
        s: &Session,
        set: &str,
        reason: &str,
    ) -> Result<(AltSetInfo, Vec<String>), RdyError> {
        need_writer(s)?;
        if reason.trim().is_empty() {
            return Err(bad(
                "abandoning an alternative set needs a reason".to_string(),
                "say in a few words why no member is chosen",
            ));
        }
        let st = self.read();
        let (stable, version, cur) = st.resolve_alt(set)?;
        need_open(&stable, &cur)?;
        st.may_change_alt(s, &stable)?;
        drop(st);
        let by = s.provenance();
        let views: Vec<String> = cur.members.iter().map(|m| m.view.clone()).collect();
        let next = AltSet {
            stable: Some(stable),
            prev: Some(version),
            status: AltStatus::Abandoned {
                reason: reason.to_string(),
            },
            provenance: by.clone(),
            ..cur
        };
        let w = self
            .store
            .alt_set_write(&next, &views, &abandoned_reason(&stable, reason), &by)?;
        self.publish(Delta::Coord {
            closed: w.closed.clone(),
        })?;
        let info = self.read().alt_info(stable, w.version, &next)?;
        Ok((info, w.closed))
    }

    /// Writes the version after `version` of set `stable`, with `members`, by the session.
    fn next_alt_version(
        &mut self,
        s: &Session,
        stable: Hash,
        version: Hash,
        cur: AltSet,
        members: Vec<AltMember>,
    ) -> Result<AltSetInfo, RdyError> {
        let next = AltSet {
            stable: Some(stable),
            prev: Some(version),
            members,
            provenance: s.provenance(),
            ..cur
        };
        let v = self.store.put_alt_set(&next)?;
        self.publish(Delta::Coord { closed: vec![] })?;
        self.read().alt_info(stable, v, &next)
    }
}
