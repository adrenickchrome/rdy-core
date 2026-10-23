//! Reviewing a passing mention (FW-CV4): a block names an entity without a link, and a writer
//! says the mention is passing (`Dismiss`) or needs a link again (`Restore`). The review is
//! stored with its actor. The latest review of a (block, entity) pair wins, so a dismissal is
//! reversible. A dismissal is not tied to a scope or to a version of the block: it holds in
//! every scope and across edits, until a restore (D-E6JD1). A block that stops naming the entity
//! has no such mention, and its stored review does nothing.
//!
//! A passing mention has no sides, so no self-review rule applies (ledger Q6).
use crate::error::*;
use crate::tensions::MAX_REASON;
use crate::{Delta, Rdy, RdyError, ReadState, Scope, Session};
use rdy_model::{BlockId, Hash, Review, ReviewSubject, Verdict};
use serde::{Deserialize, Serialize};

/// What a mention review says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionVerdict {
    /// The mention is passing: it needs no link.
    Dismiss,
    /// The mention needs a link again.
    Restore,
}

/// What a mention review returns: the pair, the verdict, the review written (64 hex) and the
/// mention's status now (`dismissed` or `open`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MentionReviewInfo {
    pub block: String,
    pub entity: String,
    pub verdict: MentionVerdict,
    pub review: String,
    pub status: String,
}

/// How a scope is named in a refusal.
fn scope_label(scope: &Scope) -> String {
    match scope {
        Scope::Canon => "canon".to_string(),
        Scope::View(v) => v.clone(),
        Scope::At(h) => format!("landing:{h}"),
        Scope::Proposal(v) => format!("proposal:{}", v.short()),
        Scope::Inflight => crate::INFLIGHT_SCOPE.to_string(),
    }
}

impl ReadState {
    /// The review that dismisses the mention of `entity` in `block`, if the latest review of the
    /// pair dismisses it.
    pub fn mention_dismissed(&self, block: &BlockId, entity: &BlockId) -> Option<Hash> {
        self.mention_dismissals
            .get(&(block.clone(), entity.clone()))
            .copied()
    }

    /// Every dismissed mention as ((block, entity), review).
    pub fn mention_dismissals(&self) -> &imbl::OrdMap<(BlockId, BlockId), Hash> {
        &self.mention_dismissals
    }

    /// Whether `block` names `entity` in `scope` without a link: the lexicon finds the entity in
    /// the block's text and the block is not tied to it.
    fn names_without_link(
        &self,
        scope: &Scope,
        block: &BlockId,
        entity: &BlockId,
    ) -> Result<bool, RdyError> {
        let Some(b) = self.materialized(scope)?.get(block).cloned() else {
            return Ok(false);
        };
        let named = self
            .lexicon(scope)?
            .mentions(&b)
            .iter()
            .any(|m| BlockId::parse(&m.entity).is_ok_and(|id| id == *entity));
        Ok(named && !crate::lexicon::tied(&*self.graph(scope)?, block, entity))
    }
}

impl Rdy {
    /// Reviews the mention of `entity` in `block` with `verdict` and `reason`: writes the review
    /// and the `mention/<block>/<entity>` ref, and publishes the review. `scope` is where the
    /// mention is checked (the caller's view). Checks, in order: the session may write; the
    /// reason is 1 to [`MAX_REASON`] characters after trimming; both ids parse; a dismissal needs
    /// a current mention without a link in `scope` that is not dismissed yet; a restore needs the
    /// pair to be dismissed now.
    pub fn review_mention(
        &mut self,
        s: &Session,
        scope: &Scope,
        block: &str,
        entity: &str,
        verdict: MentionVerdict,
        reason: &str,
    ) -> Result<MentionReviewInfo, RdyError> {
        if !s.role.can_write() {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("role {:?} cannot review mentions", s.role),
                "start the session as a writer",
            ));
        }
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > MAX_REASON {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "a mention review needs a reason of 1 to 2000 characters",
                "say in a few words why the mention is passing, or why it needs a link again",
            ));
        }
        let parse = |text: &str| {
            BlockId::parse(text).map_err(|e| {
                RdyError::new(
                    E_BAD_INPUT,
                    e.to_string(),
                    "pass the block id and the entity id from the suggestion",
                )
            })
        };
        let (block, entity) = (parse(block)?, parse(entity)?);
        let st = self.read();
        let held = st.mention_dismissed(&block, &entity);
        match (verdict, held) {
            (MentionVerdict::Dismiss, Some(review)) => {
                // an unreadable review still answers that the mention is dismissed
                let by = st
                    .reader()
                    .review(&review)
                    .map_or_else(|_| "unknown actor".to_string(), |r| r.provenance.actor);
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!(
                        "the mention of {entity} in {block} is already dismissed by review {} ({by})",
                        review.short()
                    ),
                    "restore it first if it needs a link",
                ));
            }
            (MentionVerdict::Dismiss, None) => {
                if !st.names_without_link(scope, &block, &entity)? {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        format!(
                            "{block} does not name {entity} without a link in view {}",
                            scope_label(scope)
                        ),
                        "dismiss only a mention the suggestions list; a linked mention needs no review",
                    ));
                }
            }
            (MentionVerdict::Restore, None) => {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("the mention of {entity} in {block} is not dismissed"),
                    "nothing to restore",
                ));
            }
            (MentionVerdict::Restore, Some(_)) => {}
        }
        let review = Review {
            subject: ReviewSubject::Mention {
                block: block.clone(),
                entity: entity.clone(),
            },
            case_hash: None,
            verdict: match verdict {
                MentionVerdict::Dismiss => Verdict::Dismiss,
                MentionVerdict::Restore => Verdict::Restore,
            },
            reason: reason.to_string(),
            citations: vec![],
            provenance: s.provenance(),
        };
        drop(st);
        let id = self.store_mut().put_mention_review(&review)?;
        let dismissed = verdict == MentionVerdict::Dismiss;
        self.publish(Delta::MentionReviewed {
            block: block.clone(),
            entity: entity.clone(),
            review: id,
            dismissed,
        })?;
        Ok(MentionReviewInfo {
            block: block.to_string(),
            entity: entity.to_string(),
            verdict,
            review: id.to_string(),
            status: if dismissed { "dismissed" } else { "open" }.to_string(),
        })
    }
}
