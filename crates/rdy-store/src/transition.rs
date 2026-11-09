//! Tension and suspect moves (plan M2e2, founder decision C, D-Z0EMP): each batch logs how it
//! moved the open tension set and the suspect set, so the inbox reads them from the log and
//! reindex reproduces them. No read surface lists past moves (D-KTW43, D-WDKWQ).
use crate::kinds;
use rdy_model::foreign::ForeignId;
use rdy_model::{BlockId, Hash, RepoId};
use serde::{Deserialize, Serialize};

/// One batch's moves of the open tension set and of the suspect set (kind TRANSITION; plan
/// M2e2, founder decision C, D-Z0EMP). Applied in field order: `resolved`, `opened`, `gone`,
/// `new`. Each list is sorted by id (suspects by `suspect_key`) and distinct.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transitions {
    /// When the batch was sealed: the `at_ms` of the batch's last other event, else the clock.
    pub at_ms: i64,
    pub resolved: Vec<TensionMove>,
    pub opened: Vec<TensionMove>,
    pub gone: Vec<SuspectMove>,
    pub new: Vec<SuspectMove>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TensionMove {
    #[serde(with = "hex::one")]
    pub tension: Hash,
    /// Wire names, as rdy_tensions shows them.
    pub kind: String,
    pub severity: String,
    pub sides: Vec<Side>,
    /// A re-key: on an opening, the id it replaces; on a resolution, the id that replaces it.
    #[serde(with = "hex::opt")]
    pub rekey: Option<Hash>,
    pub why: Why,
}

/// One side: an in-flight change with its author, or canon (at a landing when known).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Side {
    #[serde(with = "hex::opt")]
    pub change: Option<Hash>,
    pub actor: Option<String>,
    pub run: Option<String>,
    #[serde(with = "hex::opt")]
    pub landing: Option<Hash>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuspectMove {
    pub block: BlockId,
    pub target: BlockId,
    pub reason: String,
    /// The authors of the live in-flight writes on `block` when the move was made, sorted and
    /// distinct.
    pub writers: Vec<Author>,
    pub why: Why,
}

impl SuspectMove {
    /// This suspect's identity (`suspect_key`).
    pub fn key(&self) -> Hash {
        suspect_key(&self.block, &self.target, &self.reason)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Author {
    pub actor: String,
    pub run: Option<String>,
}

/// What made a move. `id`: the change (committed), landing (landed), review (dismissed,
/// reviewed), view op (view_changed), alternative-set version (alt_set) or FOREIGN entry
/// (delivered); none for the rest.
/// `actor` and `run`: the dismisser, for dismissed only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Why {
    pub cause: Cause,
    #[serde(with = "hex::opt")]
    pub id: Option<Hash>,
    pub actor: Option<String>,
    pub run: Option<String>,
}

/// The closed list of causes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    Committed,
    Landed,
    Dismissed,
    ViewChanged,
    Reviewed,
    AltSet,
    Written,
    PackReload,
    Reopened,
    Recheck,
    /// A delivery from a repo link (repo links, appended: index 10): `Why::id` names the
    /// FOREIGN entry. A record with a move of this cause takes TRANSITION encoding 1
    /// ([`rdy_model::encoding::TRANSITION_FOREIGN_ENCODING`]).
    Delivered,
}

impl Cause {
    /// The snake_case name, as the event JSON writes it.
    pub fn name(self) -> &'static str {
        match self {
            Cause::Committed => "committed",
            Cause::Landed => "landed",
            Cause::Dismissed => "dismissed",
            Cause::ViewChanged => "view_changed",
            Cause::Reviewed => "reviewed",
            Cause::AltSet => "alt_set",
            Cause::Written => "written",
            Cause::PackReload => "pack_reload",
            Cause::Reopened => "reopened",
            Cause::Recheck => "recheck",
            Cause::Delivered => "delivered",
        }
    }

    /// Whether a move of this cause names the object that made it (`Why::id`).
    fn has_id(self) -> bool {
        matches!(
            self,
            Cause::Committed
                | Cause::Landed
                | Cause::Dismissed
                | Cause::ViewChanged
                | Cause::Reviewed
                | Cause::AltSet
                | Cause::Delivered
        )
    }
}

impl std::fmt::Display for Cause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// `Hash::of` of `rdy-suspect-1\0{block}\0{target}\0{reason}`: one suspect's identity.
pub fn suspect_key(block: &BlockId, target: &BlockId, reason: &str) -> Hash {
    Hash::of(format!("rdy-suspect-1\0{block}\0{target}\0{reason}").as_bytes())
}

/// Whether `keys` is strictly ascending: sorted and distinct.
fn ascending<T: Ord>(keys: impl IntoIterator<Item = T>) -> bool {
    let mut prev: Option<T> = None;
    for k in keys {
        if prev.as_ref().is_some_and(|p| *p >= k) {
            return false;
        }
        prev = Some(k);
    }
    true
}

impl Transitions {
    /// No move at all.
    pub fn is_empty(&self) -> bool {
        self.resolved.is_empty()
            && self.opened.is_empty()
            && self.gone.is_empty()
            && self.new.is_empty()
    }

    /// Every move's `why`, in record order.
    fn whys(&self) -> impl Iterator<Item = &Why> {
        let tensions = self.resolved.iter().chain(&self.opened).map(|t| &t.why);
        tensions.chain(self.gone.iter().chain(&self.new).map(|s| &s.why))
    }

    /// The rule this record breaks, if any (one list, used by fsck and by Task 15's debug
    /// check before it writes a record).
    pub fn invalid(&self) -> Option<String> {
        const UNSORTED: &str = "its lists are not sorted and distinct";
        let tensions_sorted = |l: &[TensionMove]| ascending(l.iter().map(|t| t.tension));
        let suspects_sorted = |l: &[SuspectMove]| ascending(l.iter().map(SuspectMove::key));
        if !tensions_sorted(&self.resolved)
            || !tensions_sorted(&self.opened)
            || !suspects_sorted(&self.gone)
            || !suspects_sorted(&self.new)
        {
            return Some(UNSORTED.into());
        }
        let side_ok = |s: &Side| match s.change {
            Some(_) => s.actor.is_some() && s.landing.is_none(),
            None => s.actor.is_none() && s.run.is_none(),
        };
        if !self
            .resolved
            .iter()
            .chain(&self.opened)
            .all(|t| t.sides.iter().all(side_ok))
        {
            return Some("a side is neither a change with its author nor canon".into());
        }
        if let Some(w) = self.whys().find(|w| w.id.is_some() != w.cause.has_id()) {
            return Some(format!("cause {} and its id do not agree", w.cause));
        }
        let dismissed = |w: &Why| w.cause == Cause::Dismissed;
        if let Some(w) = self
            .whys()
            .find(|w| w.actor.is_some() != dismissed(w) || (w.run.is_some() && w.actor.is_none()))
        {
            return Some(format!("cause {} and its actor do not agree", w.cause));
        }
        if !self
            .gone
            .iter()
            .chain(&self.new)
            .all(|s| ascending(s.writers.iter()))
        {
            return Some(UNSORTED.into());
        }
        if self.is_empty() {
            return Some("it holds no move".into());
        }
        None
    }

    /// Whether a move has cause [`Cause::Delivered`]: the one foreign shape the runtime record
    /// holds, which takes TRANSITION encoding 1.
    pub fn has_foreign(&self) -> bool {
        self.whys().any(|w| w.cause == Cause::Delivered)
    }

    /// The frame a batch appends: its raw kind and payload, in the encoding the codec chooses
    /// for it (`crate::codec`): encoding 1 when a move was delivered, else the released one.
    pub fn frame(&self) -> (u8, Vec<u8>) {
        crate::codec::item_new(kinds::TRANSITION, self)
    }
}

/// One side of a tension move in TRANSITION encoding 1
/// ([`rdy_model::encoding::TRANSITION_FOREIGN_ENCODING`], repo links): a local side, as [`Side`]
/// holds it, or a foreign side, the cited repo and the FOREIGN entry that holds it
/// (`SideRef::Foreign {repo, entry}`, D-Y49PX). Variant order is format. A registered wire shape:
/// the runtime side stays [`Side`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SideV1 {
    Local(Side),
    Foreign {
        #[serde(with = "rdy_model::foreign::hex_repo")]
        repo: RepoId,
        #[serde(with = "hex::one")]
        entry: Hash,
    },
}

/// A tension move in TRANSITION encoding 1: [`TensionMove`]'s fields in their order, its sides as
/// [`SideV1`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TensionMoveV1 {
    #[serde(with = "hex::one")]
    pub tension: Hash,
    pub kind: String,
    pub severity: String,
    pub sides: Vec<SideV1>,
    #[serde(with = "hex::opt")]
    pub rekey: Option<Hash>,
    pub why: Why,
}

/// The target of a suspect move in TRANSITION encoding 1: a local block, as [`SuspectMove`]
/// holds it, or a cited block. Variant order is format. A registered wire shape: the runtime
/// target stays a [`BlockId`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SuspectTarget {
    Local(BlockId),
    Foreign(ForeignId),
}

/// A suspect move in TRANSITION encoding 1: [`SuspectMove`]'s fields in their order, its target
/// as [`SuspectTarget`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuspectMoveV1 {
    pub block: BlockId,
    pub target: SuspectTarget,
    pub reason: String,
    pub writers: Vec<Author>,
    pub why: Why,
}

/// One batch's moves in TRANSITION encoding 1: [`Transitions`]' fields in their order, with
/// [`TensionMoveV1`] and [`SuspectMoveV1`] moves. A TRANSITION takes it only when it holds a
/// foreign shape: a delivered cause, a foreign side or a foreign target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionsV1 {
    pub at_ms: i64,
    pub resolved: Vec<TensionMoveV1>,
    pub opened: Vec<TensionMoveV1>,
    pub gone: Vec<SuspectMoveV1>,
    pub new: Vec<SuspectMoveV1>,
}

impl TransitionsV1 {
    /// The layout of `t`, every side and target local.
    pub fn of(t: &Transitions) -> TransitionsV1 {
        let tension = |m: &TensionMove| TensionMoveV1 {
            tension: m.tension,
            kind: m.kind.clone(),
            severity: m.severity.clone(),
            sides: m.sides.iter().cloned().map(SideV1::Local).collect(),
            rekey: m.rekey,
            why: m.why.clone(),
        };
        let suspect = |m: &SuspectMove| SuspectMoveV1 {
            block: m.block.clone(),
            target: SuspectTarget::Local(m.target.clone()),
            reason: m.reason.clone(),
            writers: m.writers.clone(),
            why: m.why.clone(),
        };
        TransitionsV1 {
            at_ms: t.at_ms,
            resolved: t.resolved.iter().map(tension).collect(),
            opened: t.opened.iter().map(tension).collect(),
            gone: t.gone.iter().map(suspect).collect(),
            new: t.new.iter().map(suspect).collect(),
        }
    }

    /// Every move's `why`, in record order.
    pub fn whys(&self) -> impl Iterator<Item = &Why> {
        let tensions = self.resolved.iter().chain(&self.opened).map(|t| &t.why);
        tensions.chain(self.gone.iter().chain(&self.new).map(|s| &s.why))
    }

    /// Whether the record holds a foreign shape: the one case that takes encoding 1.
    pub fn has_foreign(&self) -> bool {
        self.whys().any(|w| w.cause == Cause::Delivered)
            || self
                .resolved
                .iter()
                .chain(&self.opened)
                .flat_map(|m| &m.sides)
                .any(|s| matches!(s, SideV1::Foreign { .. }))
            || self
                .gone
                .iter()
                .chain(&self.new)
                .any(|m| matches!(m.target, SuspectTarget::Foreign(_)))
    }

    /// The runtime record, when every side and target is local (a delivered cause has its
    /// runtime form).
    pub fn into_local(self) -> Option<Transitions> {
        let tension = |m: TensionMoveV1| -> Option<TensionMove> {
            Some(TensionMove {
                tension: m.tension,
                kind: m.kind,
                severity: m.severity,
                sides: m
                    .sides
                    .into_iter()
                    .map(|s| match s {
                        SideV1::Local(s) => Some(s),
                        SideV1::Foreign { .. } => None,
                    })
                    .collect::<Option<_>>()?,
                rekey: m.rekey,
                why: m.why,
            })
        };
        let suspect = |m: SuspectMoveV1| -> Option<SuspectMove> {
            Some(SuspectMove {
                block: m.block,
                target: match m.target {
                    SuspectTarget::Local(b) => b,
                    SuspectTarget::Foreign(_) => return None,
                },
                reason: m.reason,
                writers: m.writers,
                why: m.why,
            })
        };
        Some(Transitions {
            at_ms: self.at_ms,
            resolved: self
                .resolved
                .into_iter()
                .map(tension)
                .collect::<Option<_>>()?,
            opened: self
                .opened
                .into_iter()
                .map(tension)
                .collect::<Option<_>>()?,
            gone: self.gone.into_iter().map(suspect).collect::<Option<_>>()?,
            new: self.new.into_iter().map(suspect).collect::<Option<_>>()?,
        })
    }
}

/// Hashes as lowercase hex in a human-readable format (JSON), and as `Hash`'s own bytes
/// otherwise (postcard), so the stored bytes equal a plain derive and the event JSON matches
/// every other rdy surface.
pub mod hex {
    /// One hash.
    pub mod one {
        use rdy_model::Hash;
        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        pub fn serialize<S: Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
            if s.is_human_readable() {
                s.serialize_str(&h.to_string())
            } else {
                h.serialize(s)
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Hash, D::Error> {
            if d.is_human_readable() {
                let text = String::deserialize(d)?;
                text.parse()
                    .map_err(|_| serde::de::Error::custom(format!("bad hash {text:?}")))
            } else {
                Hash::deserialize(d)
            }
        }
    }

    /// An optional hash.
    pub mod opt {
        use rdy_model::Hash;
        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        pub fn serialize<S: Serializer>(h: &Option<Hash>, s: S) -> Result<S::Ok, S::Error> {
            if s.is_human_readable() {
                h.map(|h| h.to_string()).serialize(s)
            } else {
                h.serialize(s)
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Hash>, D::Error> {
            if d.is_human_readable() {
                Option::<String>::deserialize(d)?
                    .map(|text| {
                        text.parse()
                            .map_err(|_| serde::de::Error::custom(format!("bad hash {text:?}")))
                    })
                    .transpose()
            } else {
                Option::<Hash>::deserialize(d)
            }
        }
    }
}
