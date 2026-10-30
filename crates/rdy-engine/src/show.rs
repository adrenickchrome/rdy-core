//! `rdy show` targets (O13, D-HZYK0): what the text names, a proposal, a landing or a block.
use crate::error::*;
use crate::version::match_prefix;
use crate::{RdyError, ReadState, Scope};
use rdy_model::Hash;
use std::collections::BTreeSet;

/// What `rdy show <text>` reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShowTarget {
    /// A proposal reference (`P-<id>`, `P-<id>@<n>`) or the full hash of a proposal version.
    Proposal(String),
    /// A landing on canon's chain.
    Landing(Hash),
    /// Anything else: a block id, key or path, read as today.
    Block,
}

impl ReadState {
    /// O13, steps (1) to (5).
    pub fn show_target(&self, text: &str) -> Result<ShowTarget, RdyError> {
        // (1) a proposal ref
        if crate::is_proposal_ref(text) {
            return Ok(ShowTarget::Proposal(text.to_string()));
        }
        // (2) `landing:<id>` or `tag:<name>`
        if text.starts_with("landing:") || text.starts_with("tag:") {
            return match self.resolve_scope(text)? {
                Scope::At(h) => Ok(ShowTarget::Landing(h)),
                _ => Err(RdyError::new(
                    E_INTERNAL,
                    format!("{text:?} did not resolve to a landing"),
                    "report this as a bug in rdy",
                )),
            };
        }
        if !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(ShowTarget::Block);
        }
        // (3) a full hash: a proposal version, else a landing
        if text.len() == 64 {
            let h: Hash = text.to_ascii_lowercase().parse().map_err(|_| {
                RdyError::new(E_INTERNAL, "a hex hash did not parse", "report this bug")
            })?;
            if self.reader().has_kind(rdy_store::kinds::PROPOSAL, &h)? {
                return Ok(ShowTarget::Proposal(h.to_string()));
            }
            if self.landing_ids()?.contains(&h) {
                return Ok(ShowTarget::Landing(h));
            }
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("{text} is not a proposal version or a landing"),
                "list landings with rdy log, or pass P-<id>@<n>",
            ));
        }
        // (4) a prefix; (5) else a block
        if !(MIN_PREFIX..64).contains(&text.len()) {
            return Ok(ShowTarget::Block);
        }
        let proposals = self.proposal_versions()?;
        let landings = self.landing_ids()?;
        Ok(resolve_hex(text, &proposals, &landings)?.unwrap_or(ShowTarget::Block))
    }

    /// Every proposal version: each `proposal/` ref (`refs_with_prefix`) and its `prev` chain,
    /// in hash order.
    fn proposal_versions(&self) -> Result<Vec<Hash>, RdyError> {
        let mut found: BTreeSet<Hash> = BTreeSet::new();
        for (_, value) in self.reader().refs_with_prefix("proposal/")? {
            let mut cur = Some(crate::gate::ref_hash(&value, "a proposal ref")?);
            while let Some(h) = cur {
                if !found.insert(h) {
                    // the rest of this chain is listed already
                    break;
                }
                cur = self.reader().proposal(&h)?.prev;
            }
        }
        Ok(found.into_iter().collect())
    }
}

/// The shortest hash prefix that names a target (D-HZYK0).
const MIN_PREFIX: usize = 6;

/// O13 step (4) on lists in hand: the pure core that the unit tests drive with made-up hashes.
/// `None` when `text` is no hash prefix (under 6 characters, not hex) or starts no listed hash.
pub(crate) fn resolve_hex(
    text: &str,
    proposals: &[Hash],
    landings: &[Hash],
) -> Result<Option<ShowTarget>, RdyError> {
    if !(MIN_PREFIX..64).contains(&text.len()) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let want = text.to_ascii_lowercase();
    let starts = |h: &&Hash| h.to_string().starts_with(&want);
    let versions: Vec<Hash> = proposals.iter().filter(starts).copied().collect();
    let landed: Vec<Hash> = landings.iter().filter(starts).copied().collect();
    if !versions.is_empty() && !landed.is_empty() {
        return Err(RdyError::new(
            E_NOT_FOUND,
            format!("{text} names a proposal version and a landing"),
            "give more characters, or landing:<id>",
        ));
    }
    if !landed.is_empty() {
        // one landing, or `match_prefix`'s refusal for two or more
        return match_prefix(&landed, text).map(|h| Some(ShowTarget::Landing(h)));
    }
    match versions.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(ShowTarget::Proposal(one.to_string()))),
        many => Err(RdyError::new(
            E_NOT_FOUND,
            format!(
                "{text:?} matches {} proposal versions: {}",
                many.len(),
                many.iter()
                    .map(|h| h.short())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "give more characters of the version id, or pass P-<id>@<n>",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hash whose hex text starts with `head` and is padded with `0`.
    fn h(head: &str) -> Hash {
        format!("{head:0<64}").parse().expect("a hash")
    }

    #[test]
    fn dq4_a_prefix_of_both_kinds_is_refused() {
        let e = resolve_hex("abcdef", &[h("abcdef1")], &[h("abcdef2")]).unwrap_err();
        assert_eq!(e.code, E_NOT_FOUND);
        assert_eq!(e.message, "abcdef names a proposal version and a landing");
        assert_eq!(e.fix, "give more characters, or landing:<id>");
    }

    #[test]
    fn dq4_a_prefix_of_one_landing_is_that_landing() {
        let l = h("abcdef2");
        assert_eq!(
            resolve_hex("abcdef", &[h("123456")], &[l, h("777777")]).unwrap(),
            Some(ShowTarget::Landing(l))
        );
    }

    #[test]
    fn dq4_a_prefix_of_one_proposal_version_is_that_version() {
        let v = h("abcdef1");
        assert_eq!(
            resolve_hex("abcdef", &[v, h("123456")], &[h("777777")]).unwrap(),
            Some(ShowTarget::Proposal(v.to_string()))
        );
    }

    #[test]
    fn dq4_a_prefix_of_two_landings_keeps_match_prefixs_text() {
        let (a, b) = (h("abcdef1"), h("abcdef2"));
        let e = resolve_hex("abcdef", &[], &[a, b]).unwrap_err();
        assert_eq!(e.code, E_NOT_FOUND);
        assert_eq!(
            e.message,
            format!(
                "\"abcdef\" matches 2 landings: {}, {}",
                a.short(),
                b.short()
            )
        );
        assert_eq!(e.fix, "give more characters of the landing id");
    }

    #[test]
    fn dq4_a_prefix_of_two_proposal_versions_is_refused() {
        let (a, b) = (h("abcdef1"), h("abcdef2"));
        let e = resolve_hex("abcdef", &[a, b], &[]).unwrap_err();
        assert_eq!(e.code, E_NOT_FOUND);
        assert_eq!(
            e.message,
            format!(
                "\"abcdef\" matches 2 proposal versions: {}, {}",
                a.short(),
                b.short()
            )
        );
        assert_eq!(
            e.fix,
            "give more characters of the version id, or pass P-<id>@<n>"
        );
    }

    #[test]
    fn dq4_a_prefix_of_nothing_is_no_hash() {
        assert_eq!(
            resolve_hex("abcdef", &[h("123456")], &[h("777777")]).unwrap(),
            None
        );
    }

    #[test]
    fn dq4_five_hex_characters_are_no_hash() {
        // five characters start a hash, but are too short to name one
        assert_eq!(
            resolve_hex("abcde", &[h("abcdef1")], &[h("abcdef2")]).unwrap(),
            None
        );
        // text that is not hex names no hash either
        assert_eq!(resolve_hex("abcdeg", &[h("abcdef1")], &[]).unwrap(), None);
    }

    #[test]
    fn dq4_upper_case_hex_matches() {
        let l = h("abcdef2");
        assert_eq!(
            resolve_hex("ABCDEF", &[], &[l]).unwrap(),
            Some(ShowTarget::Landing(l))
        );
    }
}
