//! Redaction records and read identities (D-3M81W: rdy "drops their bytes and keeps their
//! hashes"). Three new kinds hold a request ([`Redaction`]), the residue of each removed block
//! version or change ([`RedactionResidue`]) and the human apply record ([`RedactionApply`]).
//! Their field and variant order is format.
//!
//! No existing kind changes its encoding (D-F0904). A removed block or change is read as a
//! projection: a stub with a transient, never serialized read identity
//! ([`Block::read_identity`], [`Change::read_identity`]) that keeps its recorded version hash or
//! change id. Projections are built only by the explicit constructors here; a normal constructor
//! sets the identity to `None`.
use crate::decision::is_frozen;
use crate::{Block, BlockId, Change, ChangeLinks, Hash, Justification, Op, Provenance};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Serializes into a caller-owned bounded buffer and returns the used slice, or postcard's
/// error when the buffer is too small. Production callers cap the buffer at
/// [`MAX_RECORD_BYTES`].
pub use postcard::to_slice as encode_into;

/// The body of a removed block: the only new content text.
pub const CONTENT_REMOVED: &str = "Content removed.";

/// The message of a request's carrier change.
pub const CARRIER_MESSAGE: &str = "Request content removal.";

/// The domain prefix of [`RedactionManifest::report_hash`].
pub const REPORT_DOMAIN: &[u8] = b"rdy-redaction-report-v1\0";

/// The largest encoded new redaction record, in bytes.
pub const MAX_RECORD_BYTES: usize = 16_777_216;

/// A position of a frame in the store's log; converted to the store's own location at the seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FramePlace {
    pub segment: u32,
    pub offset: u64,
}

/// What a request selects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RedactTarget {
    /// Every version of a block.
    AllVersions(BlockId),
    /// One version of a block.
    Version {
        id: BlockId,
        #[serde(with = "hex::one")]
        version: Hash,
    },
    /// A blob, by its manifest hash.
    Blob(#[serde(with = "hex::one")] Hash),
}

/// Why content is removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonClass {
    Secret,
    Personal,
    Legal,
    Retraction,
}

/// One frame payload the removal zeroes: where it is, its kind, its object hash and length, and
/// the residue that replaces it for reads, if any.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DropSpan {
    pub place: FramePlace,
    pub kind: u8,
    #[serde(with = "hex::one")]
    pub hash: Hash,
    pub len: u32,
    #[serde(with = "hex::opt")]
    pub residue: Option<Hash>,
}

/// A removal request (kind REDACTION). It names its carrier change, the requester, the
/// selectors, the reason and the complete canonical manifest it binds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Redaction {
    #[serde(with = "hex::one")]
    pub carrier: Hash,
    pub actor: Provenance,
    pub targets: Vec<RedactTarget>,
    pub reason_class: ReasonClass,
    pub reason: String,
    pub manifest: RedactionManifest,
}

/// The record that activates a request (kind REDACTION_APPLY): the request, the human landing it
/// shares a sealed batch with, and the report hash that landing approved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionApply {
    #[serde(with = "hex::one")]
    pub request: Hash,
    #[serde(with = "hex::one")]
    pub landing: Hash,
    #[serde(with = "hex::one")]
    pub report_hash: Hash,
}

/// The record that carries an applied removal into a store that does not hold its proof (kind
/// REDACTION_CARRIED): the request, apply and landing hashes of the source, the person who
/// approved it, its reason class and selectors, and each physical span it accounts for. It holds
/// no reason text and no new time. It explains exact already-zero bytes; it never grants erasure
/// authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionCarried {
    #[serde(with = "hex::one")]
    pub request: Hash,
    #[serde(with = "hex::one")]
    pub apply: Hash,
    #[serde(with = "hex::one")]
    pub landing: Hash,
    pub approved_by: Provenance,
    pub reason_class: ReasonClass,
    pub targets: Vec<RedactTarget>,
    pub spans: Vec<CarriedSpan>,
}

/// One physical span a carried record accounts for: where it is, its kind, its object hash and
/// length, its residue if any, and whether the store still holds the frame's bytes (`held`) or
/// holds zeros in their place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarriedSpan {
    pub place: FramePlace,
    pub kind: u8,
    #[serde(with = "hex::one")]
    pub hash: Hash,
    pub len: u32,
    #[serde(with = "hex::opt")]
    pub residue: Option<Hash>,
    pub held: bool,
}

/// What survives a removed object (kind REDACTION_RESIDUE). The variants hold their records
/// unboxed, as declared: a residue is decoded one at a time, and the declaration is the format.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RedactionResidue {
    Block(BlockResidue),
    Change(ChangeResidue),
}

/// The closed structural facts of a removed block version: no title, key, field, body, link or
/// pin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockResidue {
    #[serde(with = "hex::one")]
    pub version: Hash,
    pub id: BlockId,
    pub block_type: String,
    pub parent: Option<BlockId>,
    pub order: String,
    #[serde(with = "hex::one")]
    pub content_hash: Hash,
    pub frozen: bool,
}

/// One op of a change whose embedded blocks are named by version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ResidualOp {
    Create {
        id: BlockId,
        #[serde(with = "hex::one")]
        version: Hash,
    },
    Update {
        #[serde(with = "hex::one")]
        base: Hash,
        id: BlockId,
        #[serde(with = "hex::one")]
        version: Hash,
    },
    Delete {
        #[serde(with = "hex::one")]
        base: Hash,
        id: BlockId,
    },
}

/// A change whose embedded block bytes were removed: its original id and every audit field, with
/// each op naming its block by version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeResidue {
    #[serde(with = "hex::one")]
    pub change: Hash,
    pub view: String,
    pub ops: Vec<ResidualOp>,
    #[serde(with = "hex::vec")]
    pub deps: Vec<Hash>,
    pub provenance: Provenance,
    pub justification: Option<Justification>,
    pub message: String,
    #[serde(with = "hex::links")]
    pub links: ChangeLinks,
}

/// Whether the report can assess anything beyond the kind registry. Describes the registry, not
/// an assessment result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentSupport {
    NotImplemented,
}

/// The kind of record a report row names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactKind {
    BlockVersion,
    Change,
    Landing,
    Tag,
    View,
    Proposal,
    StateSnapshot,
    AuditText,
    Review,
    Tension,
    Answer,
}

/// How a record relates to the removed content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactRelation {
    Removed,
    Embeds,
    References,
    Quotes,
}

/// One record inside the store that the removal affects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactRow {
    pub kind: ImpactKind,
    pub record: String,
    pub relation: ImpactRelation,
    pub erasable: bool,
}

/// A kind of copy outside the store (D-14SAY).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyKind {
    Render,
    Export,
    Dump,
    Clone,
    Published,
    WorkingCopy,
}

/// What rdy knows of copies of one kind: their locations are always unknown (D-14SAY).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyStatus {
    Unknown,
}

/// One fixed row per copy kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyRow {
    pub kind: CopyKind,
    pub status: CopyStatus,
}

/// The complete canonical report a request binds. Quarantine holds only store-relative names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionManifest {
    #[serde(with = "hex::pairs")]
    pub versions: Vec<(BlockId, Hash)>,
    #[serde(with = "hex::vec")]
    pub blobs: Vec<Hash>,
    #[serde(with = "hex::vec")]
    pub shared_blobs: Vec<Hash>,
    pub spans: Vec<DropSpan>,
    pub inside: Vec<ImpactRow>,
    pub outside: Vec<CopyRow>,
    pub quarantine: Vec<String>,
    pub assessment: AssessmentSupport,
}

impl RedactionManifest {
    /// The report hash: BLAKE3 of [`REPORT_DOMAIN`] then the manifest's postcard bytes. No
    /// presentation text, token budget or time enters it.
    ///
    /// The manifest is encoded into the caller's bounded `buf` (production callers pass a
    /// [`record_buffer`] of [`MAX_RECORD_BYTES`]) and hashed from there, with no other copy. A
    /// manifest that does not fit returns postcard's `SerializeBufferFull`; callers report it as
    /// E_REDACTION_LIMIT (`16777216 bytes per record`).
    pub fn report_hash(&self, buf: &mut [u8]) -> Result<Hash, postcard::Error> {
        let bytes = encode_into(self, buf)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(REPORT_DOMAIN);
        hasher.update(bytes);
        Ok(Hash(*hasher.finalize().as_bytes()))
    }
}

/// A zeroed buffer of [`MAX_RECORD_BYTES`] for [`encode_into`] and
/// [`RedactionManifest::report_hash`], allocated fallibly. Callers report an allocation failure
/// as E_REDACTION_LIMIT (`16777216 bytes per record`).
pub fn record_buffer() -> Result<Vec<u8>, std::collections::TryReserveError> {
    let mut buf = Vec::new();
    buf.try_reserve_exact(MAX_RECORD_BYTES)?;
    buf.resize(MAX_RECORD_BYTES, 0);
    Ok(buf)
}

/// A warning shown with a redaction report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionWarning {
    pub code: String,
    pub message: String,
    pub fix: String,
}

/// The transient read identity of a removed block version: never serialized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedactedBlockIdentity {
    /// The removed version's recorded version hash.
    pub version: Hash,
    /// The removed version's recorded content hash (with no tracking fields left out).
    pub content_hash: Hash,
    /// Whether the removed version was an accepted or rejected decision.
    pub frozen: bool,
    /// The request that removed it.
    pub request: Hash,
}

impl Block {
    /// The read projection of a removed block version: its id, type, parent and order, body
    /// [`CONTENT_REMOVED`], no fields, links or pins, and its recorded identity.
    pub fn redacted(
        id: BlockId,
        block_type: String,
        parent: Option<BlockId>,
        order: String,
        identity: RedactedBlockIdentity,
    ) -> Block {
        Block {
            id,
            block_type,
            parent,
            order,
            fields: BTreeMap::new(),
            body: CONTENT_REMOVED.to_string(),
            links: BTreeMap::new(),
            pins: BTreeMap::new(),
            read_identity: Some(identity),
        }
    }

    /// Whether this block is the read projection of a removed version.
    pub fn is_redacted(&self) -> bool {
        self.read_identity.is_some()
    }

    /// The request that removed this block's content, if it was removed.
    pub fn redaction(&self) -> Option<Hash> {
        self.read_identity.as_ref().map(|r| r.request)
    }
}

impl BlockResidue {
    /// The residue of `block`: its recorded version and content hashes, structure and frozen fact.
    pub fn of(block: &Block) -> BlockResidue {
        BlockResidue {
            version: block.version_hash(),
            id: block.id.clone(),
            block_type: block.block_type.clone(),
            parent: block.parent.clone(),
            order: block.order.clone(),
            content_hash: block.content_hash(),
            frozen: is_frozen(block),
        }
    }

    /// The read projection of this residue, removed by `request`.
    pub fn stub(&self, request: Hash) -> Block {
        Block::redacted(
            self.id.clone(),
            self.block_type.clone(),
            self.parent.clone(),
            self.order.clone(),
            RedactedBlockIdentity {
                version: self.version,
                content_hash: self.content_hash,
                frozen: self.frozen,
                request,
            },
        )
    }
}

impl Change {
    /// Whether this change is the read projection of a residue.
    pub fn is_projection(&self) -> bool {
        self.read_identity.is_some()
    }
}

impl ChangeResidue {
    /// The residue of `change`: its id, its audit fields, and each op naming its block by version.
    pub fn of(change: &Change) -> ChangeResidue {
        let ops = change
            .ops
            .iter()
            .map(|op| match op {
                Op::Create { block } => ResidualOp::Create {
                    id: block.id.clone(),
                    version: block.version_hash(),
                },
                Op::Update { base, block } => ResidualOp::Update {
                    base: *base,
                    id: block.id.clone(),
                    version: block.version_hash(),
                },
                Op::Delete { base, id } => ResidualOp::Delete {
                    base: *base,
                    id: id.clone(),
                },
            })
            .collect();
        ChangeResidue {
            change: change.id(),
            view: change.view.clone(),
            ops,
            deps: change.deps.clone(),
            provenance: change.provenance.clone(),
            justification: change.justification.clone(),
            message: change.message.clone(),
            links: change.links.clone(),
        }
    }

    /// The read projection of this residue. `resolve` returns the block of each op's (id,
    /// original version hash): the original block when it survives, or its stub. The projection
    /// keeps the recorded change id, op order, bases and every audit field.
    pub fn project<E>(
        &self,
        mut resolve: impl FnMut(&BlockId, Hash) -> Result<Block, E>,
    ) -> Result<Change, E> {
        let mut ops = Vec::with_capacity(self.ops.len());
        for op in &self.ops {
            ops.push(match op {
                ResidualOp::Create { id, version } => Op::Create {
                    block: resolve(id, *version)?,
                },
                ResidualOp::Update { base, id, version } => Op::Update {
                    base: *base,
                    block: resolve(id, *version)?,
                },
                ResidualOp::Delete { base, id } => Op::Delete {
                    base: *base,
                    id: id.clone(),
                },
            });
        }
        Ok(Change {
            view: self.view.clone(),
            ops,
            deps: self.deps.clone(),
            provenance: self.provenance.clone(),
            justification: self.justification.clone(),
            message: self.message.clone(),
            links: self.links.clone(),
            read_identity: Some(self.change),
        })
    }
}

/// A request's carrier: an ordinary change with no ops, the view's existing dependencies, the
/// requester's provenance, no justification, [`CARRIER_MESSAGE`] and default links.
pub fn carrier(view: String, deps: Vec<Hash>, provenance: Provenance) -> Change {
    Change {
        view,
        ops: vec![],
        deps,
        provenance,
        justification: None,
        message: CARRIER_MESSAGE.to_string(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// Hashes as full lowercase hex in a human-readable format, and as `Hash`'s own bytes otherwise
/// (postcard), so the stored bytes equal a plain derive. The same algorithm as the store's
/// transition records; the model cannot depend on the store.
mod hex {
    use crate::Hash;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    fn parse<E: serde::de::Error>(text: &str) -> Result<Hash, E> {
        text.parse()
            .map_err(|_| E::custom(format!("bad hash {text:?}")))
    }

    /// One hash.
    pub mod one {
        use super::*;

        pub fn serialize<S: Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
            if s.is_human_readable() {
                s.serialize_str(&h.to_string())
            } else {
                h.serialize(s)
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Hash, D::Error> {
            if d.is_human_readable() {
                parse(&String::deserialize(d)?)
            } else {
                Hash::deserialize(d)
            }
        }
    }

    /// An optional hash.
    pub mod opt {
        use super::*;

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
                    .map(|t| parse(&t))
                    .transpose()
            } else {
                Option::<Hash>::deserialize(d)
            }
        }
    }

    /// A list of hashes.
    pub mod vec {
        use super::*;

        pub fn serialize<S: Serializer>(h: &[Hash], s: S) -> Result<S::Ok, S::Error> {
            if s.is_human_readable() {
                s.collect_seq(h.iter().map(Hash::to_string))
            } else {
                h.serialize(s)
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Hash>, D::Error> {
            if d.is_human_readable() {
                Vec::<String>::deserialize(d)?
                    .iter()
                    .map(|t| parse(t))
                    .collect()
            } else {
                Vec::<Hash>::deserialize(d)
            }
        }
    }

    /// A list of (block id, hash) pairs.
    pub mod pairs {
        use super::*;
        use crate::BlockId;

        pub fn serialize<S: Serializer>(h: &[(BlockId, Hash)], s: S) -> Result<S::Ok, S::Error> {
            if s.is_human_readable() {
                s.collect_seq(h.iter().map(|(id, h)| (id, h.to_string())))
            } else {
                h.serialize(s)
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Vec<(BlockId, Hash)>, D::Error> {
            if d.is_human_readable() {
                Vec::<(BlockId, String)>::deserialize(d)?
                    .into_iter()
                    .map(|(id, t)| Ok((id, parse(&t)?)))
                    .collect()
            } else {
                Vec::<(BlockId, Hash)>::deserialize(d)
            }
        }
    }

    /// A change's links: the intent and alternative-set hashes as hex in a human-readable
    /// format; `ChangeLinks`'s own serializer, unchanged, otherwise (postcard).
    pub mod links {
        use super::*;
        use crate::{ActorRun, AltChoice, ChangeLinks};

        /// `ChangeLinks`'s fields, in its order, with hex hashes.
        #[derive(Serialize)]
        #[serde(rename = "ChangeLinks")]
        struct LinksOut<'a> {
            #[serde(with = "super::opt")]
            intent: Option<Hash>,
            alternative: Option<AltOut<'a>>,
            client_nonce: &'a Option<String>,
            imported_by: &'a Option<ActorRun>,
        }

        #[derive(Serialize)]
        #[serde(rename = "AltChoice")]
        struct AltOut<'a> {
            #[serde(with = "super::one")]
            set: Hash,
            member: &'a str,
        }

        #[derive(Deserialize)]
        #[serde(rename = "ChangeLinks")]
        struct LinksIn {
            // `default`: an omitted key reads as None, as in ChangeLinks's own derive; a
            // field-level `with` alone would make the key mandatory.
            #[serde(default, with = "super::opt")]
            intent: Option<Hash>,
            alternative: Option<AltIn>,
            client_nonce: Option<String>,
            imported_by: Option<ActorRun>,
        }

        #[derive(Deserialize)]
        #[serde(rename = "AltChoice")]
        struct AltIn {
            #[serde(with = "super::one")]
            set: Hash,
            member: String,
        }

        pub fn serialize<S: Serializer>(l: &ChangeLinks, s: S) -> Result<S::Ok, S::Error> {
            if s.is_human_readable() {
                LinksOut {
                    intent: l.intent,
                    alternative: l.alternative.as_ref().map(|a| AltOut {
                        set: a.set,
                        member: &a.member,
                    }),
                    client_nonce: &l.client_nonce,
                    imported_by: &l.imported_by,
                }
                .serialize(s)
            } else {
                l.serialize(s)
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<ChangeLinks, D::Error> {
            if d.is_human_readable() {
                let l = LinksIn::deserialize(d)?;
                Ok(ChangeLinks {
                    intent: l.intent,
                    alternative: l.alternative.map(|a| AltChoice {
                        set: a.set,
                        member: a.member,
                    }),
                    client_nonce: l.client_nonce,
                    imported_by: l.imported_by,
                })
            } else {
                ChangeLinks::deserialize(d)
            }
        }
    }
}
