//! Detached signatures (kind `SIGNATURE`, D-1Z951): one record per signature, which names its
//! subject, a landing or a tag. A signature never changes its subject's bytes (D-F0904), so many
//! signatures of one subject coexist, of every statement kind and key.
//!
//! The store checks structure only: the bounded record encoding (`rdy_model::signature`), the
//! subject's presence and kind, and that no key binding statement is stored. The engine checks
//! the cryptography and the statement's coverage before it writes. The index table `signatures`
//! maps `subject || signature hash` to nothing, derived from the frames alone.
use crate::error::StoreError;
use crate::kinds;
use crate::store::{Store, StoreReader};
use rdy_model::Hash;
use rdy_model::signature::{
    MAX_SIGNATURE, Signature, StatementError, StatementKind, decode_signature, parse_statement,
};

/// The most signatures of one subject one read returns.
pub const MAX_PER_SUBJECT: usize = 4096;

/// The refusal of a new input over `limit`, with its unit.
fn over(limit: impl std::fmt::Display) -> StoreError {
    StoreError::Refused(format!("signing input exceeds {limit}"))
}

/// The refusal of a read over [`MAX_PER_SUBJECT`].
pub(crate) fn over_per_subject() -> StoreError {
    over(format_args!("{MAX_PER_SUBJECT} signatures per subject"))
}

/// The error of a stored SIGNATURE whose bytes do not decode within its bounds.
pub(crate) fn invalid_record(hash: &Hash) -> StoreError {
    StoreError::Decode(format!("signature {hash} has an invalid record"))
}

/// Decodes the stored record `hash`; any failure is [`invalid_record`].
pub(crate) fn decode_stored(hash: &Hash, payload: &[u8]) -> Result<Signature, StoreError> {
    decode_signature(payload).map_err(|_| invalid_record(hash))
}

/// Decodes a new record: a bound it breaks is a refusal that names the bound, any other
/// failure is [`invalid_record`]. A key binding is refused.
fn decode_new(hash: &Hash, payload: &[u8]) -> Result<Signature, StoreError> {
    let g = decode_signature(payload).map_err(|e| match e {
        StatementError::Limit(n) => over(format_args!("{n} bytes")),
        _ => invalid_record(hash),
    })?;
    if is_keybind(&g) {
        return Err(StoreError::Refused(
            "key binding signatures are not stored".into(),
        ));
    }
    Ok(g)
}

/// Whether `g` holds a key binding: by its namespace, or by its statement's kind.
fn is_keybind(g: &Signature) -> bool {
    g.namespace == StatementKind::Keybind.namespace()
        || parse_statement(&g.statement).is_ok_and(|p| p.kind() == StatementKind::Keybind)
}

/// The frames of `signatures`, in order, for a caller that already owns a batch: each record is
/// encoded and checked against its bounds, and a key binding is refused. The write checks each
/// subject against the store and the batch's preceding frames.
pub fn frames(signatures: &[Signature]) -> Result<Vec<(u8, Vec<u8>)>, StoreError> {
    let mut out = Vec::new();
    out.try_reserve_exact(signatures.len())
        .map_err(|_| over(format_args!("{MAX_SIGNATURE} bytes")))?;
    for g in signatures {
        let body = encode_bounded(g)?;
        let (kind, payload) = crate::codec::frame_latest(kinds::SIGNATURE, body);
        let o =
            crate::codec::split(kind, &payload).map_err(|_| invalid_record(&Hash::of(&payload)))?;
        decode_new(&Hash::of(&payload), o.body)?;
        out.push((kind, payload));
    }
    Ok(out)
}

/// The postcard encoding of `g`, refused before it is built when a field is over its bound.
fn encode_bounded(g: &Signature) -> Result<Vec<u8>, StoreError> {
    use rdy_model::signature::{MAX_NAMESPACE, MAX_SIG, MAX_SIGNER_TEXT, MAX_STATEMENT};
    let fields = [
        (g.namespace.len(), MAX_NAMESPACE),
        (g.statement.len(), MAX_STATEMENT),
        (g.sig.len(), MAX_SIG),
        (g.signed_by.actor.len(), MAX_SIGNER_TEXT),
        (
            g.signed_by.run.as_ref().map_or(0, String::len),
            MAX_SIGNER_TEXT,
        ),
    ];
    if let Some((_, limit)) = fields.iter().find(|(len, limit)| len > limit) {
        return Err(over(format_args!("{limit} bytes")));
    }
    let bytes = postcard::to_allocvec(g).map_err(|e| StoreError::Decode(e.to_string()))?;
    if bytes.len() > MAX_SIGNATURE {
        return Err(over(format_args!("{MAX_SIGNATURE} bytes")));
    }
    Ok(bytes)
}

impl StoreReader {
    /// Checks the SIGNATURE frames of a new batch `items`, before anything is appended: each
    /// decodes within its bounds, holds no key binding and names a subject that is a LANDING or
    /// a TAG, stored already (an earlier write of an open held batch included) or among the
    /// frames of `items` before it.
    pub(crate) fn check_new_signatures(&self, items: &[(u8, Vec<u8>)]) -> Result<(), StoreError> {
        for (i, (kind, payload)) in items.iter().enumerate() {
            if crate::codec::logical_kind(*kind) != kinds::SIGNATURE {
                continue;
            }
            let body = crate::codec::split(*kind, payload)
                .map_err(|_| invalid_record(&Hash::of(payload)))?
                .body;
            if body.len() > MAX_SIGNATURE {
                return Err(over(format_args!("{MAX_SIGNATURE} bytes")));
            }
            let g = decode_new(&Hash::of(payload), body)?;
            let earlier = items[..i].iter().any(|(k, p)| {
                matches!(crate::codec::logical_kind(*k), kinds::LANDING | kinds::TAG)
                    && Hash::of(p) == g.subject
            });
            if !earlier
                && !self.has_kind(kinds::LANDING, &g.subject)?
                && !self.has_kind(kinds::TAG, &g.subject)?
            {
                return Err(StoreError::Refused(format!(
                    "signature subject {} is not a present landing or tag",
                    g.subject
                )));
            }
        }
        Ok(())
    }

    /// The signature `hash`, decoded. A stored record that does not decode within its bounds is
    /// `Decode("signature {hash} has an invalid record")`.
    pub fn signature(&self, hash: &Hash) -> Result<Signature, StoreError> {
        decode_stored(hash, &self.get_kind(kinds::SIGNATURE, hash)?)
    }

    /// The signatures of `subject`, in byte order of their hashes: at most
    /// [`MAX_PER_SUBJECT`]. One more is `Refused("signing input exceeds 4096 signatures per
    /// subject")`, found without collecting it.
    pub fn signatures_of(&self, subject: &Hash) -> Result<Vec<Hash>, StoreError> {
        self.index().signatures_of(subject, MAX_PER_SUBJECT)
    }

    /// The signatures of `subject` whose frames lie before `end` (a published state's log end),
    /// in byte order of their hashes: [`StoreReader::signatures_of`] less the frames of a batch
    /// that is still open or was sealed after that state. The bound counts only the signatures
    /// returned.
    pub fn signatures_before(
        &self,
        subject: &Hash,
        end: crate::segment::Location,
    ) -> Result<Vec<Hash>, StoreError> {
        self.index()
            .signatures_before(subject, MAX_PER_SUBJECT, end)
    }

    /// Calls `f` with every stored signature, decoded, in subject then hash order, one at a
    /// time: the whole set is never collected.
    pub fn visit_signatures(
        &self,
        f: &mut dyn FnMut(Hash, &Signature) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.index()
            .for_each_signature(&mut |h| f(h, &self.signature(&h)?))
    }
}

impl Store {
    /// Writes `signature` as one detached record in its own batch. Refused before anything is
    /// written: a field or the record over its bound (`signing input exceeds {limit}`), a key
    /// binding, or a subject that is not a stored landing or tag.
    pub fn put_signature(&mut self, signature: &Signature) -> Result<Hash, StoreError> {
        let items = frames(std::slice::from_ref(signature))?;
        Ok(self.write(items)?[0])
    }

    /// Drops the `signatures` index table in a durable commit. No store path calls it: it is the
    /// test hook of an index that lacks the table, which the next open rebuilds from the log.
    #[doc(hidden)]
    pub fn drop_signature_table_for_tests(&mut self) -> Result<(), StoreError> {
        self.index().drop_signature_table_for_tests()
    }
}
