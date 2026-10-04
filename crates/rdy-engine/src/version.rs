//! Versions (addendum A): immutable SemVer tags on canon's landings, created only by an approver.
use crate::error::*;
use crate::{Rdy, Session};
use rdy_model::{Hash, Tag};
use serde::Serialize;
use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VersionInfo {
    pub name: String,
    /// The full hash of the landing the version names.
    pub landing: String,
    pub message: Option<String>,
    pub tagged_by: String,
    pub at_ms: i64,
}

fn info(t: &Tag) -> VersionInfo {
    VersionInfo {
        name: t.name.clone(),
        landing: t.landing.to_string(),
        message: t.message.clone(),
        tagged_by: t.tagged_by.actor.clone(),
        at_ms: t.tagged_by.at_ms,
    }
}

/// Parses a version name: one optional leading `v`, then a SemVer 2.0.0 version without build
/// metadata (SemVer ignores build metadata when ordering, so two versions would share a position).
pub fn parse_version(name: &str) -> Result<semver::Version, RdyError> {
    let bad = |why: String| {
        RdyError::new(
            E_BAD_TAG_NAME,
            format!("{name:?} is not a version: {why}"),
            "name versions like v1.2.0 or 2.0.0-rc.1 (Semantic Versioning, one optional leading v, no +build metadata)",
        )
    };
    let bare = name.strip_prefix('v').unwrap_or(name);
    let v = semver::Version::parse(bare).map_err(|e| bad(e.to_string()))?;
    if !v.build.is_empty() {
        return Err(bad(
            "build metadata (+…) is not allowed: it doesn't take part in version order".into(),
        ));
    }
    Ok(v)
}

/// The candidates `text` names: a full hash, or a unique hex prefix of at least 6 characters.
pub(crate) fn match_prefix(candidates: &[Hash], text: &str) -> Result<Hash, RdyError> {
    let want = text.to_ascii_lowercase();
    if want.len() < 6 || !want.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(RdyError::new(
            E_NOT_FOUND,
            format!("{text:?} is not a landing id"),
            "pass a landing hash, or at least 6 of its leading hex characters (see rdy log)",
        ));
    }
    let hits: Vec<Hash> = candidates
        .iter()
        .copied()
        .filter(|h| h.to_string().starts_with(&want))
        .collect();
    match hits.as_slice() {
        [one] => Ok(*one),
        [] => Err(RdyError::new(
            E_NOT_FOUND,
            format!("no landing in canon's history matches {text:?}"),
            "list landings with rdy log",
        )),
        many => Err(RdyError::new(
            E_NOT_FOUND,
            format!(
                "{text:?} matches {} landings: {}",
                many.len(),
                many.iter()
                    .map(|h| h.short())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "give more characters of the landing id",
        )),
    }
}

impl Rdy {
    /// Every version as (parsed version, tag), in SemVer precedence order. Tag refs whose name is
    /// not a version are skipped here; `rdy fsck` reports them.
    pub(crate) fn version_tags(&self) -> Result<Vec<(semver::Version, Tag)>, RdyError> {
        let mut out: Vec<(semver::Version, Tag)> = self
            .store
            .tags()?
            .into_iter()
            .filter_map(|(_, _, t)| parse_version(&t.name).ok().map(|v| (v, t)))
            .collect();
        out.sort_by(|a, b| a.0.cmp_precedence(&b.0));
        Ok(out)
    }

    /// Every version, oldest first (precedence order, which the order rules make canon order).
    pub fn versions(&self) -> Result<Vec<VersionInfo>, RdyError> {
        Ok(self.version_tags()?.iter().map(|(_, t)| info(t)).collect())
    }

    /// A landing in canon's chain, by full hash or unique prefix of at least 6 hex characters.
    pub fn resolve_landing(&self, text: &str) -> Result<Hash, RdyError> {
        let chain: Vec<Hash> = self.store.landings()?.into_iter().map(|(h, _)| h).collect();
        match_prefix(&chain, text)
    }

    /// Names a landing (default: canon's head) as version `name`. Approver only; the version must
    /// be new, above every existing version, and on the current version's landing or a later one.
    pub fn tag(
        &mut self,
        s: &Session,
        name: &str,
        landing: Option<&str>,
        message: Option<String>,
    ) -> Result<VersionInfo, RdyError> {
        if !s.role.can_land() {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("role {:?} cannot create versions", s.role),
                "a version is a release statement: an approver creates it with rdy tag",
            ));
        }
        let version = parse_version(name)?;
        let chain: Vec<Hash> = self.store.landings()?.into_iter().map(|(h, _)| h).collect();
        let target = match landing {
            Some(text) => match_prefix(&chain, text)?,
            None => *chain.first().ok_or_else(|| {
                RdyError::new(
                    E_NOT_FOUND,
                    "canon has no landings to version",
                    "land a proposal first",
                )
            })?,
        };
        let existing = self.version_tags()?;
        if let Some((_, t)) = existing
            .iter()
            .find(|(v, _)| v.cmp_precedence(&version) == Ordering::Equal)
        {
            return Err(RdyError::new(
                E_TAG_EXISTS,
                format!(
                    "version {name} already exists as {} (landing {})",
                    t.name,
                    t.landing.short()
                ),
                "versions are immutable: choose the next version number",
            ));
        }
        if let Some((current, t)) = existing.last() {
            let refuse = |why: &str| {
                RdyError::new(
                    E_VERSION_ORDER,
                    format!(
                        "{name} {why}: the current version is {} at landing {}",
                        t.name,
                        t.landing.short()
                    ),
                    format!(
                        "name a version above {} on landing {} or a later one",
                        t.name,
                        t.landing.short()
                    ),
                )
            };
            if version.cmp_precedence(current) != Ordering::Greater {
                return Err(refuse("is not above the current version"));
            }
            // chain is head first: a smaller index is a later landing
            let pos = |h: &Hash| chain.iter().position(|c| c == h);
            match (pos(&target), pos(&t.landing)) {
                (Some(new), Some(cur)) if new <= cur => {}
                _ => return Err(refuse("names an earlier landing than the current version")),
            }
        }
        let tag = Tag {
            name: name.to_string(),
            landing: target,
            message,
            tagged_by: s.provenance(),
        };
        self.store.put_tag(&tag)?;
        Ok(info(&tag))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ambiguous_prefix_lists_the_candidates() {
        let mut a = [0xab; 32];
        let mut b = [0xab; 32];
        a[31] = 1;
        b[31] = 2;
        let e = match_prefix(&[Hash(a), Hash(b)], "abababab").unwrap_err();
        assert_eq!(e.code, E_NOT_FOUND);
        assert!(e.message.contains("matches 2 landings"), "{e}");
        assert_eq!(
            match_prefix(&[Hash(a), Hash(b)], &Hash(a).to_string()).unwrap(),
            Hash(a)
        );
    }
}
