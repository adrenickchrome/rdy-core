//! The `adopted_as` branch of supersede (repo links, D-8VJAZ "Adopted upstream"): a local
//! decision accepted in its home project and delivered back is superseded by its adopted copy.
//!
//! - It is apart from the `decision` branch: it takes the old decision and the cited decision,
//!   no decision input. `rdy_supersede {old, adopted_as}` and `rdy adopt-upstream` call it.
//! - The old decision is a local accepted decision in force. The cited decision is a decision
//!   that the alias's current profile selection delivers.
//! - The write records the store-only engine field `adopted_as` on the old decision: the cited
//!   decision and the delivered content checked here. Nothing else of the old decision changes,
//!   and the cited decision is never written. The store takes it under its own frozen-decision
//!   exception, which checks the recorded successor again on every commit, replay and landing.
//! - The old decision's status becomes `superseded`, by the cited decision, and the version's
//!   plan gives its dependents their items. A version that writes `adopted_as` never lands under
//!   the authority policy.
use crate::error::*;
use crate::foreign_refs::ForeignCite;
use crate::write::{OpInput, WriteRequest, WriteResult};
use crate::{Rdy, Scope, Session};
use rdy_graph::DerivedStatus;
use rdy_model::decision::{self, DecisionStatus};
use rdy_model::engine_fields::AdoptedAs;

/// The fix of an `adopted_as` that names no cited decision.
const TARGET_FIX: &str =
    "name the cited decision as alias:id: rdy_links {op: \"show\", alias} / rdy links show <alias>";

impl Rdy {
    /// Supersedes the accepted decision `old` (an id) in `view` by its adopted copy `adopted_as`,
    /// the cited decision written `alias:id`, `alias:KEY` or the stored `<repo>:<id>`, citing the
    /// briefing `brief` and under `intent`, as any write does. Refused, with nothing written:
    /// - `old` is cited or external (`E_FOREIGN_READ_ONLY`), or no decision (`E_BAD_INPUT`);
    /// - `old` is proposed or rejected, or is no longer in force (`E_BAD_INPUT`);
    /// - `adopted_as` names no cited block (`E_BAD_INPUT`), an unknown alias or a block the
    ///   alias does not deliver (`E_UNKNOWN_REF`), or a cited block that is no decision
    ///   (`E_BAD_INPUT`);
    /// - and every check of the write itself.
    pub fn supersede_adopted_as(
        &mut self,
        s: &Session,
        view: &str,
        old: &str,
        adopted_as: &str,
        brief: Option<String>,
        intent: Option<String>,
    ) -> Result<WriteResult, RdyError> {
        let old_id = self.require_decision(view, old)?;
        let scope = Scope::View(view.into());
        let adopted = {
            let st = self.read();
            let b = st.block(&scope, &old_id)?;
            match decision::status(&b) {
                Some(DecisionStatus::Accepted) => {}
                other => {
                    let word = match other {
                        Some(DecisionStatus::Rejected) => "rejected",
                        _ => "proposed",
                    };
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        format!(
                            "{old_id} is {word}, not accepted, so it has no upstream copy to adopt"
                        ),
                        "adopt an accepted decision; supersede any other with rdy_supersede and a decision",
                    ));
                }
            }
            match st.graph(&scope)?.status(&old_id) {
                Some(s @ (DerivedStatus::Superseded { .. } | DerivedStatus::Revoked { .. })) => {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        format!("{old_id} is {} already", s.tag()),
                        "adopt a decision that is in force",
                    ));
                }
                _ => {}
            }
            let m = st.materialized(&scope)?;
            let cite = ForeignCite::new(&st, &m);
            let text = adopted_as.trim();
            let not_cited = || {
                RdyError::new(
                    E_BAD_INPUT,
                    format!("adopted_as names {text}, which is not a cited decision"),
                    TARGET_FIX,
                )
            };
            let target = cite.link_target(text).ok_or_else(not_cited)??;
            let pin = cite.new_pin(&target)?;
            let version = st.foreign_held_pin(&target, &pin)?.ok_or_else(not_cited)?;
            let cited = st.reader().get_block(&version)?;
            if cited.block_type != decision::DECISION {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("{text} is a {}, not a decision", cited.block_type),
                    format!("name the cited decision that supersedes {old_id}"),
                ));
            }
            AdoptedAs { target, pin }
        };
        let message = format!(
            "adopt upstream {old_id}: {}",
            rdy_model::reference::foreign_token(&adopted.target.repo, &adopted.target.id)
        );
        self.write_with_intent(
            s,
            WriteRequest {
                view: view.into(),
                ops: vec![OpInput::AdoptUpstream {
                    id: old_id,
                    adopted,
                }],
                justification: None,
                message,
            },
            brief,
            intent.as_deref(),
            None,
        )
    }
}
