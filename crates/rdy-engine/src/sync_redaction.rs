//! Removals through sync (D-FNRXT, D-CC90E, D-3M81W): the uncovered-carrier warning and the
//! removed content a pull replay must not copy.
//!
//! A fetch takes each removal of the history it brings when a person's approval proves it, and
//! erases what that removal covers. A local change that holds a removed block version, and that
//! no removal covers, keeps its bytes. [`ReadState::uncovered_warnings`] warns once per such
//! change, on every read: it is derived from the store's mask and producers, so no
//! acknowledgement is stored, and it repeats after a reopen or a reindex. The warning ends when
//! a person approves a removal here that covers the change.
//!
//! [`removed_in`] lists the removals that hold content of a landing's changes. A pull replay
//! stops at such a landing (`StopReason::Removal`), and the `pull-<n>` view never gets those
//! changes: their text is never copied into a new change or a conflict. For each block of
//! removed content they write, the view gets a mark change instead ([`mark_change`]: no op, no
//! link, no text), and the view records a conflict on that block that names the removal
//! ([`removal_conflicts`]). The conflict is derived from the view's changes and the mask on
//! every read, so it stays across a reopen and a reindex, and it ends when the person writes
//! the block in the view again.
use crate::error::{SYNC_UNCOVERED_FIX, SYNC_UNCOVERED_MESSAGE, W_REDACTION_UNCOVERED};
use crate::{RdyError, ReadState};
use rdy_model::{BlockId, Change, ChangeLinks, Hash, LandingRecord, Op, Provenance};
use rdy_store::StoreReader;
use rdy_store::kinds;
use rdy_store::merge::{Conflict, Side, conflict_id};
use rdy_store::transition::hex;
use std::collections::{BTreeSet, HashMap, HashSet};

/// A local change that holds content a removal removed, and that no removal covers. Not stored.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct UncoveredWarning {
    pub code: &'static str,
    pub message: String,
    pub fix: String,
    /// The local change that keeps its bytes.
    #[serde(with = "hex::one")]
    pub change: Hash,
    /// The removal request that removed the content.
    #[serde(with = "hex::one")]
    pub removal: Hash,
}

/// The warning of change `change`, whose content removal `removal` removed.
pub fn uncovered_warning(change: Hash, removal: Hash) -> UncoveredWarning {
    UncoveredWarning {
        code: W_REDACTION_UNCOVERED,
        message: SYNC_UNCOVERED_MESSAGE
            .replace("<change>", &change.to_string())
            .replace("<removal>", &removal.to_string()),
        fix: SYNC_UNCOVERED_FIX.to_string(),
        change,
        removal,
    }
}

impl ReadState {
    /// One warning per uncovered local carrier, in ascending change order (see the module docs).
    pub fn uncovered_warnings(&self) -> Result<Vec<UncoveredWarning>, RdyError> {
        Ok(self
            .reader()
            .uncovered_carriers()?
            .into_iter()
            .map(|(change, removal)| uncovered_warning(change, removal))
            .collect())
    }
}

/// The removals that hold content of landing `l`'s changes, in ascending order: the request
/// that removed a change, or a block version one of its ops produces. Empty when none does.
pub fn removed_in(reader: &StoreReader, l: &LandingRecord) -> Result<Vec<Hash>, RdyError> {
    let mut out: BTreeSet<Hash> = BTreeSet::new();
    for c in &l.changes {
        if let Some(r) = reader.redaction_of(kinds::CHANGE, c)? {
            out.insert(r);
            continue;
        }
        // a carrier holds no removed text: its request is the landing's own removal
        if reader.redaction_for_carrier(c)?.is_some() {
            continue;
        }
        for op in &reader.change(c)?.ops {
            if let Op::Create { block } | Op::Update { block, .. } = op
                && let Some(r) = reader.redaction_of(kinds::BLOCK, &block.version_hash())?
            {
                out.insert(r);
            }
        }
    }
    Ok(out.into_iter().collect())
}

/// A block of removed content a pull did not replay (D-FNRXT): the block, the removal that
/// removed it, and the version the change wrote, which reads as removed. It holds no text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemovalMark {
    pub(crate) block: BlockId,
    pub(crate) removal: Hash,
    pub(crate) version: Hash,
}

/// The prefix of a removal mark's `client_nonce` ([`mark_change`]).
const MARK_PREFIX: &str = "removed:";

/// The blocks of change `c` whose content a removal removed, each with that removal and the
/// version its op writes, in op order: every block of a removed change, else each block whose
/// produced version is removed. A delete holds no content and gives none.
pub(crate) fn removed_ops(reader: &StoreReader, c: &Hash) -> Result<Vec<RemovalMark>, RdyError> {
    // a removed change reads as its projection, which keeps its block ids and no text
    let whole = reader.redaction_of(kinds::CHANGE, c)?;
    let mut out = vec![];
    for op in &reader.change(c)?.ops {
        let (Op::Create { block } | Op::Update { block, .. }) = op else {
            continue;
        };
        let version = block.version_hash();
        let removal = match whole {
            Some(r) => Some(r),
            None => reader.redaction_of(kinds::BLOCK, &version)?,
        };
        if let Some(removal) = removal {
            out.push(RemovalMark {
                block: block.id.clone(),
                removal,
                version,
            });
        }
    }
    Ok(out)
}

/// The change that records mark `m` in view `view` (a pull's `pull-<n>`), by `by`: no op, no
/// dependency and no link, so it copies no text and reaches no change that holds removed
/// content; its `client_nonce` names the removal, the removed version and the block, as the
/// engine's replays name their place there.
pub(crate) fn mark_change(view: &str, m: &RemovalMark, by: &Provenance) -> Change {
    Change {
        view: view.to_string(),
        ops: vec![],
        deps: vec![],
        provenance: by.clone(),
        justification: None,
        message: String::new(),
        links: ChangeLinks {
            client_nonce: Some(format!(
                "{MARK_PREFIX}{}:{}:{}",
                m.removal, m.version, m.block
            )),
            ..ChangeLinks::default()
        },
        read_identity: None,
    }
}

/// The mark change `c` records, when it is one: no op, and a `client_nonce` that
/// [`mark_change`] wrote.
fn mark_of(c: &Change) -> Option<RemovalMark> {
    if !c.ops.is_empty() || c.links.replayed_from.is_some() {
        return None;
    }
    let rest = c.links.client_nonce.as_deref()?.strip_prefix(MARK_PREFIX)?;
    let mut parts = rest.splitn(3, ':');
    let removal = parts.next()?.parse().ok()?;
    let version = parts.next()?.parse().ok()?;
    let block = BlockId::parse(parts.next()?).ok()?;
    Some(RemovalMark {
        block,
        removal,
        version,
    })
}

/// The conflicts a view records on blocks of removed content (D-FNRXT): for each mark change of
/// `included` (in the view's order; [`mark_change`]) whose version reads as removed, one
/// conflict on its block, until a later change of `included` writes the block. Its sides are
/// the block's version in the view (`version_of`, with the view's producer of it, else canon's)
/// and the removed version, by the mark. No side holds text: the removed side reads as removed
/// by its removal, which `crate::views::conflict_infos` names. Blocks that already have a
/// conflict in `merged` get none. `changes` holds the changes already read.
pub(crate) fn removal_conflicts(
    reader: &StoreReader,
    included: &[Hash],
    changes: &HashMap<Hash, Change>,
    version_of: &dyn Fn(&BlockId) -> Option<Hash>,
    merged: &[Conflict],
) -> Result<Vec<Conflict>, RdyError> {
    if included.is_empty() || !reader.any_removal() {
        return Ok(vec![]);
    }
    let taken: HashSet<&BlockId> = merged.iter().map(|c| &c.block).collect();
    let mut open: Vec<(BlockId, Hash, Hash)> = vec![];
    for h in included {
        let read;
        let c = match changes.get(h) {
            Some(c) => c,
            None => {
                read = reader.change(h)?;
                &read
            }
        };
        // a later write of a block resolves its conflict
        open.retain(|(b, _, _)| !c.ops.iter().any(|op| op.block_id() == b));
        if let Some(m) = mark_of(c)
            && !taken.contains(&m.block)
            && !open.iter().any(|o| o.0 == m.block)
            && reader.redaction_of(kinds::BLOCK, &m.version)?.is_some()
        {
            open.push((m.block, *h, m.version));
        }
    }
    let members: HashSet<&Hash> = included.iter().collect();
    let mut out = vec![];
    for (block, mark, removed) in open {
        let base = version_of(&block);
        // the view's own producer of its version, else the one canon reads
        let current = match base {
            Some(v) => match reader
                .producers(&v)?
                .into_iter()
                .find(|p| members.contains(p))
            {
                Some(p) => Some(p),
                None => reader.producer(&v)?,
            },
            None => None,
        };
        let mut sides = vec![
            Side {
                change: current,
                version: base,
            },
            Side {
                change: Some(mark),
                version: Some(removed),
            },
        ];
        sides.sort();
        out.push(Conflict {
            id: conflict_id(&block, base, &sides),
            block,
            base,
            sides,
        });
    }
    out.sort_by(|a, b| a.block.cmp(&b.block));
    Ok(out)
}
