//! The queue item (D-JV6Z8): one shape on every surface.

use crate::error::{E_BAD_INPUT, RdyError};
use rdy_model::Hash;
use rdy_model::redaction::ReasonClass;
use serde::{Deserialize, Serialize};
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
}

impl ItemKind {
    /// The word the card shows at the right of its first line.
    pub fn word(self) -> &'static str {
        match self {
            ItemKind::Approve => "approve",
            ItemKind::Confirm => "confirm",
            ItemKind::Answer => "answer",
            ItemKind::Resolve => "resolve",
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
}

impl ExceptionKind {
    /// Every kind, in the order the docs list them.
    pub const ALL: [ExceptionKind; 5] = [
        ExceptionKind::Against,
        ExceptionKind::OneWay,
        ExceptionKind::Unapplied,
        ExceptionKind::NotRecommended,
        ExceptionKind::BlockingTension,
    ];

    /// The name on the wire.
    pub fn name(self) -> &'static str {
        match self {
            ExceptionKind::Against => "against",
            ExceptionKind::OneWay => "one_way",
            ExceptionKind::Unapplied => "unapplied",
            ExceptionKind::NotRecommended => "not_recommended",
            ExceptionKind::BlockingTension => "blocking_tension",
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
    /// The reason for recommending this option. Some marks the option, even with an empty reason; label holds its words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended: Option<String>,
}

/// One computed fact, shown as a named line on the card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Fact {
    pub name: &'static str,
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
