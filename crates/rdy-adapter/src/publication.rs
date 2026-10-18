//! Publication profile and disclosure reads over one captured snapshot (D-KDFZ3, D-WDKWQ).
//!
//! Selection, the reference check and the disclosure comparison are Publications' own
//! (`rdy_engine::publication`); this module only pins them to a captured tag and shapes what a
//! host needs. It adds no selector and no text of its own beyond the prior-identity refusal.
//! Which publication was last made is the host's record: it passes that record in.

use crate::store::Snapshot;
use rdy_engine::publication::{IMPLICIT_ORG, Resolved, disclosure_diff, resolve};
use rdy_engine::{E_BAD_INPUT, E_INTERNAL, Scope};
use rdy_model::{BlockId, Hash, tree};
use rdy_schema::publication::PROFILE_TYPE;

pub use rdy_engine::RdyError;
pub use rdy_engine::publication::{Audience, DisclosureDiff, ReferenceReport};

/// One publication profile as a tag holds it. `selector` is the value to pass back to
/// [`Snapshot::disclosure`]: the full profile id, or `implicit:org`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileSummary {
    pub selector: String,
    /// None for the implicit profile.
    pub id: Option<BlockId>,
    /// The profile block's version at the tag; None for the implicit profile.
    pub version: Option<Hash>,
    /// The profile's key, or `implicit:org`.
    pub name: String,
    pub title: String,
    pub audience: Audience,
    /// The selected docs, in id order.
    pub docs: Vec<BlockId>,
    /// The selected folders (chosen or kept as ancestors), in id order.
    pub folders: Vec<BlockId>,
    pub selection_hash: Hash,
}

/// What the host recorded about the previous publication of one profile: the version it was made
/// at and the identity that publication read back then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviousPublication {
    pub tag: String,
    /// The profile block's version then; None for the implicit profile.
    pub profile_version: Option<Hash>,
    pub selection_hash: Hash,
}

/// The check of one candidate publication: its profile, its reference check, and what it
/// discloses beyond the previous publication. `comparison` is None when there is no previous
/// publication; that is not an empty diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationCheck {
    pub profile: ProfileSummary,
    pub references: ReferenceReport,
    pub comparison: Option<DisclosureDiff>,
}

/// Publications' refusal when the baseline holds no such profile (`diff.rs`); the baseline cannot
/// be resolved, so `disclosure_diff` never sees it.
fn missing_baseline_profile() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "Disclosure comparison requires the same publication profile.",
        "Choose a baseline that contains the candidate profile id.",
    )
}

fn identity_mismatch(tag: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("previous publication does not match version {tag}"),
        "read that version's profile and selection hash again",
    )
}

fn summarize(r: &Resolved) -> Result<ProfileSummary, RdyError> {
    let (selector, name) = match (&r.profile.id, &r.profile.key) {
        (None, _) => (IMPLICIT_ORG.to_string(), IMPLICIT_ORG.to_string()),
        (Some(id), Some(key)) => (id.to_string(), key.clone()),
        (Some(_), None) => {
            return Err(RdyError::new(
                E_INTERNAL,
                "a publication profile has no key",
                "report this as a bug in rdy",
            ));
        }
    };
    let mut docs: Vec<BlockId> = r.docs.iter().map(|d| d.id.clone()).collect();
    docs.sort();
    let mut folders: Vec<BlockId> = r
        .entries
        .iter()
        .filter(|e| r.m.get(&e.id).is_some_and(|b| b.block_type == tree::FOLDER))
        .map(|e| e.id.clone())
        .collect();
    folders.sort();
    Ok(ProfileSummary {
        selector,
        id: r.profile.id.clone(),
        version: r.profile.version,
        name,
        title: r.profile.title.clone(),
        audience: r.profile.audience,
        docs,
        folders,
        selection_hash: r.selection_hash,
    })
}

impl Snapshot {
    /// The scope of the captured tag `tag`: its landing, never a live tag-name lookup.
    fn tag_scope(&self, tag: &str) -> Result<Scope, RdyError> {
        let (_, captured) = self.captured_tag(tag)?;
        Ok(Scope::At(captured.landing))
    }

    /// The publication profiles at the captured tag `tag`: `implicit:org` first, then every
    /// profile block by (key, id). Each profile is the revision the tag holds.
    pub fn profiles(&self, tag: &str) -> Result<Vec<ProfileSummary>, RdyError> {
        let scope = self.tag_scope(tag)?;
        let read = self.read_state();
        let mut ids: Vec<BlockId> = read
            .materialized(&scope)?
            .iter()
            .filter(|b| b.block_type == PROFILE_TYPE)
            .map(|b| b.id.clone())
            .collect();
        ids.sort();
        let mut out = vec![summarize(&resolve(read, &scope, IMPLICIT_ORG)?)?];
        let mut real = Vec::with_capacity(ids.len());
        for id in ids {
            real.push(summarize(&resolve(read, &scope, id.as_str())?)?);
        }
        real.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
        out.extend(real);
        Ok(out)
    }

    /// Checks publishing `profile` (a profile id, a key, or `implicit:org`) at the captured tag
    /// `tag`, against `previous` when the host holds one. The previous tag is read by the
    /// candidate's profile id, so a key that was reassigned since cannot pick another profile. The
    /// host's record must match what that tag holds, or the comparison is refused.
    pub fn disclosure(
        &self,
        tag: &str,
        profile: &str,
        previous: Option<&PreviousPublication>,
    ) -> Result<PublicationCheck, RdyError> {
        let scope = self.tag_scope(tag)?;
        let read = self.read_state();
        let candidate = resolve(read, &scope, profile)?;
        let comparison = match previous {
            None => None,
            Some(prev) => {
                let prior_scope = self.tag_scope(&prev.tag)?;
                let selector = match &candidate.profile.id {
                    Some(id) => {
                        if read.materialized(&prior_scope)?.get(id).is_none() {
                            return Err(missing_baseline_profile());
                        }
                        id.to_string()
                    }
                    None => IMPLICIT_ORG.to_string(),
                };
                let prior = resolve(read, &prior_scope, &selector)?;
                if prior.profile.version != prev.profile_version
                    || prior.selection_hash != prev.selection_hash
                {
                    return Err(identity_mismatch(&prev.tag));
                }
                Some(disclosure_diff(&prior, &candidate)?)
            }
        };
        Ok(PublicationCheck {
            profile: summarize(&candidate)?,
            references: candidate.references,
            comparison,
        })
    }
}
