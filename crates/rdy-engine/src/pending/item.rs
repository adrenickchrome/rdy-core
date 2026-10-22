//! The queue item (D-JV6Z8): one shape on every surface.

use super::plan::PlanCardBinding;
use crate::card_words::REASON_MAX;
use crate::error::{E_BAD_INPUT, RdyError};
use rdy_model::Hash;
use rdy_model::redaction::ReasonClass;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::str::FromStr;

/// What the person is asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    /// Approve a proposal version (a batch).
    Approve,
    /// Confirm one decision that stands beside a batch.
    Confirm,
    /// Answer an open question.
    Answer,
    /// Resolve tensions.
    Resolve,
    /// Accept or refuse the deferrals a proposal version carries (D-AAQ9Z).
    Defer,
    /// Re-affirm the decisions a changed block made suspect, or keep them suspect (D-CQMA4).
    Reaffirm,
}

impl ItemKind {
    /// The word the card shows at the right of its first line.
    pub fn word(self) -> &'static str {
        match self {
            ItemKind::Approve => "approve",
            ItemKind::Confirm => "confirm",
            ItemKind::Answer => "answer",
            ItemKind::Resolve => "resolve",
            ItemKind::Defer => "defer",
            ItemKind::Reaffirm => "reaffirm",
        }
    }
}

/// Why a decision stands beside its batch on its own card (D-T5S9A, D-DKJS5). The set is closed:
/// the card's text for a kind is an exhaustive match, so a new kind fails to compile until its
/// words exist. A name from outside is parsed once with [`ExceptionKind::from_str`] or serde and
/// refused there; the card never meets an unknown kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExceptionKind {
    /// It supersedes or revokes a live decision, or is a side of an open tension with it.
    Against,
    /// Its reversibility is `one_way`.
    OneWay,
    /// A proposed decision no content applies.
    Unapplied,
    /// The proposer does not recommend it.
    NotRecommended,
    /// A blocking tension involves it.
    BlockingTension,
    /// The version re-affirms a `one_way` decision (D-GV465).
    ReaffirmOneWay,
}

impl ExceptionKind {
    /// Every kind, in the order the docs list them.
    pub const ALL: [ExceptionKind; 6] = [
        ExceptionKind::Against,
        ExceptionKind::OneWay,
        ExceptionKind::Unapplied,
        ExceptionKind::NotRecommended,
        ExceptionKind::BlockingTension,
        ExceptionKind::ReaffirmOneWay,
    ];

    /// The name on the wire.
    pub fn name(self) -> &'static str {
        match self {
            ExceptionKind::Against => "against",
            ExceptionKind::OneWay => "one_way",
            ExceptionKind::Unapplied => "unapplied",
            ExceptionKind::NotRecommended => "not_recommended",
            ExceptionKind::BlockingTension => "blocking_tension",
            ExceptionKind::ReaffirmOneWay => "reaffirm_one_way",
        }
    }
}

impl FromStr for ExceptionKind {
    type Err = RdyError;

    /// The exact wire name, or `E_BAD_INPUT`.
    fn from_str(s: &str) -> Result<Self, RdyError> {
        ExceptionKind::ALL
            .into_iter()
            .find(|k| k.name() == s)
            .ok_or_else(|| {
                let names: Vec<&str> = ExceptionKind::ALL.iter().map(|k| k.name()).collect();
                RdyError::new(
                    E_BAD_INPUT,
                    format!("unknown exception kind {s}"),
                    format!("use one of: {}", names.join(", ")),
                )
            })
    }
}

/// One answer the person may give.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ItemOption {
    /// The id an answer binds to (`approve`, `reject`, `later`, `o1`, ...).
    pub id: String,
    /// The label the card shows.
    pub label: String,
    /// What this answer does, one sentence a line.
    pub consequences: Vec<String>,
    /// The recommendation, on the recommended option only (D-D8FAB). It serializes as two
    /// fields, `recommended: true` and `reason`. An option without it has neither field.
    #[serde(flatten)]
    pub recommended: Option<Recommended>,
}

/// The mark of the recommended option and its reason (D-D8FAB). On the wire it is
/// `recommended: true` and `reason`. The reason is trimmed and holds at most [`REASON_MAX`]
/// characters: a longer one is cut and ends with `…`. It can be empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recommended {
    reason: String,
}

impl Recommended {
    /// The mark with `reason`, trimmed and cut to [`REASON_MAX`] characters.
    pub fn new(reason: &str) -> Self {
        let reason = reason.trim();
        let reason = if reason.chars().count() <= REASON_MAX {
            reason.to_string()
        } else {
            let kept: String = reason.chars().take(REASON_MAX - 1).collect();
            format!("{}…", kept.trim_end())
        };
        Recommended { reason }
    }

    /// Why the option is recommended.
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl Serialize for Recommended {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Recommended", 2)?;
        st.serialize_field("recommended", &true)?;
        st.serialize_field("reason", &self.reason)?;
        st.end()
    }
}

/// One computed fact, shown as a named line on the card. Most names are fixed words; a deferral
/// card names each item it lists (`<subject> (after <cause>)`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Fact {
    pub name: Cow<'static, str>,
    pub value: String,
}

/// One item of the queue, with its card.
///
/// `card` and `card_hash` are what [`crate::pending::render`] and
/// [`crate::pending::card_hash`] give for the other fields. Rendering never reads them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Item {
    pub r#ref: String,
    pub kind: ItemKind,
    pub title: String,
    pub why: String,
    pub blocking: bool,
    pub summary: Option<String>,
    pub summary_by: Option<String>,
    pub facts: Vec<Fact>,
    /// One line a decision of the batch: `D-0931  Guest readers need no account`.
    pub members: Vec<String>,
    pub options: Vec<ItemOption>,
    pub raised_by: String,
    pub raised_at: String,
    pub age: String,
    /// The batch ref an exception stands beside.
    pub batch: Option<String>,
    /// The exception kind, when this item stands beside a batch.
    pub exception: Option<ExceptionKind>,
    pub card: String,
    pub card_hash: String,
    /// The removal requests the version carries, one card each, by request hash (D-CC90E,
    /// D-6B1W1). None for every other item; omitted from JSON then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removal: Option<Vec<RemovalCard>>,
    /// The quarantine names each report of `removal` lists, by report hash. Presentation only:
    /// never serialized and not in a card's encoding; the card text it decides is bound.
    #[serde(skip)]
    pub removal_reports: BTreeMap<Hash, Vec<String>>,
    /// What a plan card binds beyond its text (O10): every member and reason, also those the
    /// text leaves out. None for every other item; omitted from JSON then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_binding: Option<PlanCardBinding>,
}

/// The facts of one removal request a version carries, as its approval card shows and binds
/// them: the request, the hash of the complete report it binds, the counts and the reason. These
/// nine fields, in this order, are the card digest's encoding. The report's quarantine names are
/// not on the card: the report hash binds them, and the card text shows their warning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemovalCard {
    #[serde(with = "rdy_store::transition::hex::one")]
    pub request: Hash,
    #[serde(with = "rdy_store::transition::hex::one")]
    pub report: Hash,
    pub versions: u64,
    pub blobs: u64,
    pub shared_blobs: u64,
    pub payloads: u64,
    pub bytes: u64,
    pub reason_class: ReasonClass,
    pub reason: String,
}

const DAY_MS: i64 = 86_400_000;

/// How long ago `raised_ms` was, in plain words: `today`, `1 day`, `<n> days` (up to 13), then
/// `<n> weeks`. A time in the future reads `today`.
pub fn age(raised_ms: i64, now_ms: i64) -> String {
    let days = now_ms.saturating_sub(raised_ms).max(0) / DAY_MS;
    match days {
        0 => "today".to_string(),
        1 => "1 day".to_string(),
        2..=13 => format!("{days} days"),
        _ => format!("{} weeks", days / 7),
    }
}
