//! The shared values of repo links and external decisions: a foreign id, the foreign parts of a
//! block, the pins that keep an approved version, and the entries of the one foreign log. The
//! field order and the variant order of every type here is format. Hashes and repo ids read and
//! write as hex text in JSON and as raw bytes in postcard. The JSON readers refuse unknown keys
//! and name the nearest known key.
use crate::did_you_mean::nearest;
use crate::{BlockId, Hash, RepoId};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;

/// The domain prefix of an ADR source id.
pub const ADR_SOURCE_DOMAIN: &[u8] = b"rdy-adr-source-1\0";

/// The domain prefix of the publication selection identity, the NUL included.
pub const SELECTION_IDENTITY_DOMAIN: &[u8] = b"rdy-publication-selection-identity\0";

/// The id of an ADR source: the first 16 bytes of the BLAKE3 hash of the domain and the source
/// name. A renamed source is a new source.
pub fn adr_source_id(name: &str) -> RepoId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(ADR_SOURCE_DOMAIN);
    hasher.update(name.as_bytes());
    let full = hasher.finalize();
    let mut out = [0u8; 16];
    out.copy_from_slice(&full.as_bytes()[..16]);
    RepoId(out)
}

/// The publication selection identity (D-075QD, D-C94DV) of `entries`, the (id, version) pairs
/// of a complete selection in id order: [`SELECTION_IDENTITY_DOMAIN`], u32 LE 1, u32 LE entry
/// count, then per entry its u32 LE id length, its UTF-8 id bytes and its 32 raw version bytes.
/// Nothing else enters. A delivery's `selection` is this identity. From repo links on it is a
/// stored, frozen format: its domain, field widths and order never change.
///
/// `None` when the count or an id length does not fit a u32; the caller reports its own limit.
pub fn selection_identity<'a, I>(entries: I) -> Option<Hash>
where
    I: IntoIterator<Item = (&'a BlockId, &'a Hash)>,
    I::IntoIter: ExactSizeIterator,
{
    let entries = entries.into_iter();
    let count = u32::try_from(entries.len()).ok()?;
    let mut h = blake3::Hasher::new();
    h.update(SELECTION_IDENTITY_DOMAIN);
    h.update(&1u32.to_le_bytes());
    h.update(&count.to_le_bytes());
    for (id, version) in entries {
        let id = id.as_str().as_bytes();
        let len = u32::try_from(id.len()).ok()?;
        h.update(&len.to_le_bytes());
        h.update(id);
        h.update(&version.0);
    }
    Some(Hash(*h.finalize().as_bytes()))
}

/// A shell of a delivery (D-2CG1N): a planned doc or section the profile keeps only as a stub.
/// It is a selection entry (its id and full source version hash count in the selection identity,
/// as they do in the publication's) and nothing else: no `BLOCK` object travels for it, and no
/// pin, ref or read resolves to it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignShell {
    pub id: BlockId,
    #[serde(with = "hex_one")]
    pub version: Hash,
}

/// The selection identity of a delivery: the delivered versions plus its `shells`, in id order.
/// This is the publication selection identity of the profile at the tag (D-2CG1N).
///
/// `shells` must be in strict id order and share no id with `delivered`. Errors name the broken
/// rule; `Err("limit")` is a count or an id length that does not fit a u32.
pub fn delivery_selection<'a, I>(
    delivered: I,
    shells: &'a [ForeignShell],
) -> Result<Hash, &'static str>
where
    I: IntoIterator<Item = (&'a BlockId, &'a Hash)>,
{
    if shells.windows(2).any(|w| w[0].id >= w[1].id) {
        return Err("shells are not in strict id order");
    }
    let mut all: Vec<(&BlockId, &Hash)> = delivered.into_iter().collect();
    all.extend(shells.iter().map(|s| (&s.id, &s.version)));
    all.sort_by(|a, b| a.0.cmp(b.0));
    if all.windows(2).any(|w| w[0].0 == w[1].0) {
        return Err("a shell shares its id with a delivered version");
    }
    selection_identity(all).ok_or("limit")
}

/// A block in another repo: that repo's id and the block's id there.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ForeignId {
    pub repo: RepoId,
    pub id: BlockId,
}

impl ForeignId {
    pub fn new(repo: RepoId, id: BlockId) -> ForeignId {
        ForeignId { repo, id }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForeignIdParts {
    repo: RepoId,
    id: BlockId,
}

impl fmt::Display for ForeignId {
    /// `<32 hex digits>/<block id>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.repo, self.id)
    }
}

impl Serialize for ForeignId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            s.serialize_str(&self.to_string())
        } else {
            ForeignIdParts {
                repo: self.repo,
                id: self.id.clone(),
            }
            .serialize(s)
        }
    }
}

impl<'de> Deserialize<'de> for ForeignId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<ForeignId, D::Error> {
        use serde::de::Error;
        if d.is_human_readable() {
            let text = String::deserialize(d)?;
            let bad = || D::Error::custom(format!("bad foreign id {text:?}"));
            let (repo, id) = text.split_once('/').ok_or_else(bad)?;
            Ok(ForeignId {
                repo: repo.parse().map_err(|_| bad())?,
                id: BlockId::parse(id).map_err(|_| bad())?,
            })
        } else {
            let p = ForeignIdParts::deserialize(d)?;
            Ok(ForeignId {
                repo: p.repo,
                id: p.id,
            })
        }
    }
}

/// A link to a block in another repo, with an optional part name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignLink {
    pub target: ForeignId,
    pub part: Option<String>,
}

/// A pin: the content identity of the cited version and the encoding of that content hash. The
/// held BLOCK version is found through the foreign index; a content hash is never compared with
/// a BLOCK version hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignPin {
    #[serde(with = "hex_one")]
    pub content: Hash,
    pub encoding: u16,
}

/// The foreign side of a block: its links by link kind, and its pins by foreign id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignParts {
    pub links: BTreeMap<String, Vec<ForeignLink>>,
    pub pins: BTreeMap<ForeignId, ForeignPin>,
}

impl ForeignParts {
    /// No foreign link list and no pin: the part of every local block, which keeps the block's
    /// encoding-0 layout and its content-2 hash.
    pub fn is_empty(&self) -> bool {
        self.links.is_empty() && self.pins.is_empty()
    }
}

/// The status and lifecycle a delivery derived for a version, and the ids that supersede it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignDerived {
    pub status: Option<String>,
    pub lifecycle: Option<String>,
    pub superseded_by: Vec<BlockId>,
}

/// One delivered version of a block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignVersion {
    pub id: BlockId,
    #[serde(with = "hex_one")]
    pub version: Hash,
    #[serde(with = "hex_one")]
    pub content: Hash,
    pub encoding: u16,
    pub derived: ForeignDerived,
}

/// The publication a delivery came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignPublication {
    pub tag: String,
    pub profile: String,
    #[serde(with = "hex_opt")]
    pub profile_version: Option<Hash>,
}

/// The set a delivery serves. An absent set is `None` at the delivery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ForeignSet {
    Organization,
    Team(String),
}

/// Where an entry came from. A source names provenance, not a core actor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ForeignSource {
    Link {
        link: String,
        site: String,
        at_ms: i64,
    },
    Adr {
        source: String,
        revision: String,
    },
}

/// What an entry records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
// a delivery is the common entry; boxing it would change every constructor and match for no gain
#[allow(clippy::large_enum_variant)]
pub enum ForeignEvent {
    Delivery {
        #[serde(default, with = "hex_opt")]
        prev: Option<Hash>,
        publication: ForeignPublication,
        set: Option<ForeignSet>,
        added: Vec<ForeignVersion>,
        changed: Vec<ForeignVersion>,
        removed: Vec<BlockId>,
        /// Every shell of this delivery, in id order; empty when the profile shows every
        /// container. Not a delta: the complete list as of this delivery.
        #[serde(default)]
        shells: Vec<ForeignShell>,
        /// The publication selection identity of the complete selection after this delivery:
        /// the delivered versions plus `shells` ([`delivery_selection`]).
        #[serde(with = "hex_one")]
        selection: Hash,
    },
    Revoked {
        #[serde(with = "hex_one")]
        prev: Hash,
    },
    /// Reserved: a foreign removal and the versions it dropped.
    Removal {
        #[serde(with = "hex_one")]
        proof: Hash,
        #[serde(with = "hex_versions")]
        versions: Vec<(BlockId, Hash)>,
    },
}

/// One entry of the foreign log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignEntry {
    #[serde(with = "hex_repo")]
    pub repo: RepoId,
    pub source: ForeignSource,
    pub event: ForeignEvent,
}

/// A refused JSON input.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ForeignInputError {
    #[error("unknown key `{key}`; did you mean `{suggestion}`?")]
    UnknownKeyDidYouMean { key: String, suggestion: String },
    #[error("unknown key `{key}`; known keys: {known}")]
    UnknownKey { key: String, known: String },
    #[error("bad foreign input: {0}")]
    Bad(String),
}

/// Reads one foreign value from JSON text. An unknown key is refused with the nearest known key.
pub fn from_json<T: DeserializeOwned>(text: &str) -> Result<T, ForeignInputError> {
    serde_json::from_str(text).map_err(|e| {
        let msg = e.to_string();
        match unknown_field(&msg) {
            Some((key, known)) => {
                match nearest(&key, &known.iter().map(String::as_str).collect::<Vec<_>>()) {
                    Some(s) => ForeignInputError::UnknownKeyDidYouMean {
                        key,
                        suggestion: s.to_string(),
                    },
                    None => ForeignInputError::UnknownKey {
                        key,
                        known: known.join(", "),
                    },
                }
            }
            None => ForeignInputError::Bad(msg),
        }
    })
}

/// The key and the known keys of serde's "unknown field `k`, expected one of `a`, `b`" message.
fn unknown_field(msg: &str) -> Option<(String, Vec<String>)> {
    let rest = msg.strip_prefix("unknown field `")?;
    let (key, rest) = rest.split_once('`')?;
    let rest = rest.strip_prefix(", expected")?;
    let rest = rest.strip_prefix(" one of").unwrap_or(rest);
    let rest = rest.split(" at line").next().unwrap_or(rest);
    let known = rest
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect::<Vec<_>>();
    Some((key.to_string(), known))
}

/// Hex text in JSON, raw bytes in postcard (shared with the records that name foreign values).
pub mod hex_one {
    use crate::Hash;
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

/// Hex text in JSON, raw bytes in postcard (shared with the records that name foreign values).
pub mod hex_opt {
    use crate::Hash;
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
                .map(|t| {
                    t.parse()
                        .map_err(|_| serde::de::Error::custom(format!("bad hash {t:?}")))
                })
                .transpose()
        } else {
            Option::<Hash>::deserialize(d)
        }
    }
}

mod hex_versions {
    use crate::{BlockId, Hash};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &[(BlockId, Hash)], s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            v.iter()
                .map(|(id, h)| (id, h.to_string()))
                .collect::<Vec<_>>()
                .serialize(s)
        } else {
            v.serialize(s)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<(BlockId, Hash)>, D::Error> {
        if d.is_human_readable() {
            Vec::<(BlockId, String)>::deserialize(d)?
                .into_iter()
                .map(|(id, t)| {
                    t.parse()
                        .map(|h| (id, h))
                        .map_err(|_| serde::de::Error::custom(format!("bad hash {t:?}")))
                })
                .collect()
        } else {
            Vec::<(BlockId, Hash)>::deserialize(d)
        }
    }
}

/// Hex text in JSON, raw bytes in postcard (shared with the records that name foreign values).
pub mod hex_repo {
    use crate::RepoId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(r: &RepoId, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            s.serialize_str(&r.to_string())
        } else {
            r.serialize(s)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<RepoId, D::Error> {
        if d.is_human_readable() {
            let text = String::deserialize(d)?;
            text.parse()
                .map_err(|_| serde::de::Error::custom(format!("bad repo id {text:?}")))
        } else {
            RepoId::deserialize(d)
        }
    }
}
