//! Versions (addendum A): immutable SemVer tags on canon's landings, created only by an approver.
use crate::error::*;
use crate::{Rdy, Scope, Session};
use rdy_model::{Hash, LandingRecord, Tag};
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::HashMap;

/// How many as-of (`tag:`/`landing:`) snapshots, with their graphs, stay cached at once.
pub const AS_OF_CACHE_ENTRIES: usize = 4;

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VersionRef {
    pub name: String,
    pub landing: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ViewPosition {
    pub name: String,
    pub unlanded_changes: u32,
}

/// Where a scope stands relative to the versions: shown by rdy status, MCP and every rendered page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VersionPosition {
    /// The highest version at or before the scope's base landing.
    pub version: Option<VersionRef>,
    /// Landings after that version, up to and including the base landing.
    pub landings_since: u32,
    /// For a view: its name and its changes not yet in canon.
    pub view: Option<ViewPosition>,
    /// The scope is an as-of snapshot (tag: or landing:), which never changes.
    pub frozen: bool,
    pub label: String,
}

fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn label_of(
    version: &Option<VersionRef>,
    since: u32,
    view: &Option<ViewPosition>,
    frozen: bool,
) -> String {
    let mut out = match version {
        None => "no version yet".to_string(),
        Some(v) if since == 0 => v.name.clone(),
        Some(v) => format!("{} + {}", v.name, plural(since, "landing", "landings")),
    };
    if let Some(v) = view {
        out.push_str(&format!(
            " · view {}, {}",
            v.name,
            plural(v.unlanded_changes, "unlanded change", "unlanded changes")
        ));
    }
    if frozen {
        out.push_str(" · frozen snapshot");
    }
    out
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
    /// Checks every `tag/*` ref (see FsckReport); `chain` is canon's landings, head first.
    pub(crate) fn check_tags(&self, chain: &[Hash]) -> Result<usize, RdyError> {
        let mut problems = vec![];
        let mut good: Vec<(semver::Version, String, usize)> = vec![];
        for (refname, value) in self.store.refs_with_prefix("tag/")? {
            let Ok(bytes) = <[u8; 32]>::try_from(value.as_slice()) else {
                problems.push(format!("{refname}: the ref is not a 32-byte hash"));
                continue;
            };
            let h = Hash(bytes);
            let tag = match self.store.read_tag(&h) {
                Ok(t) => t,
                Err(rdy_store::StoreError::NotFound(_)) => {
                    problems.push(format!(
                        "{refname} names {}, which is not in the store",
                        h.short()
                    ));
                    continue;
                }
                Err(e) => {
                    problems.push(format!(
                        "{refname} names {}, which is not a tag: {e}",
                        h.short()
                    ));
                    continue;
                }
            };
            if refname["tag/".len()..] != tag.name {
                problems.push(format!("{refname} holds the tag {:?}", tag.name));
                continue;
            }
            let Ok(v) = parse_version(&tag.name) else {
                problems.push(format!("{refname}: {:?} is not a valid version", tag.name));
                continue;
            };
            let Some(pos) = chain.iter().position(|c| *c == tag.landing) else {
                problems.push(format!(
                    "{refname}: landing {} is not in canon's chain",
                    tag.landing.short()
                ));
                continue;
            };
            good.push((v, refname, pos));
        }
        good.sort_by(|a, b| a.0.cmp_precedence(&b.0));
        for pair in good.windows(2) {
            let ((lo, lo_ref, lo_pos), (hi, hi_ref, hi_pos)) = (&pair[0], &pair[1]);
            if lo.cmp_precedence(hi) == Ordering::Equal {
                problems.push(format!("{lo_ref} and {hi_ref} denote the same version"));
            } else if hi_pos > lo_pos {
                problems.push(format!("{hi_ref} names an earlier landing than {lo_ref}"));
            }
        }
        if problems.is_empty() {
            return Ok(good.len());
        }
        Err(RdyError::new(
            E_IO,
            format!(
                "{} version problem(s): {}",
                problems.len(),
                problems.join("; ")
            ),
            "a tag ref is damaged or was written outside rdy tag: restore the store from a backup",
        ))
    }

    /// Every version as (parsed version, tag), in SemVer precedence order. Tag refs whose name is
    /// not a version, and refs that cannot be read as a tag, are skipped here so one damaged ref
    /// does not fail every read; `rdy fsck` reports them.
    pub(crate) fn version_tags(&self) -> Result<Vec<(semver::Version, Tag)>, RdyError> {
        let mut out: Vec<(semver::Version, Tag)> = self
            .store
            .refs_with_prefix("tag/")?
            .into_iter()
            .filter_map(|(_, value)| {
                let bytes = <[u8; 32]>::try_from(value.as_slice()).ok()?;
                let t = self.store.read_tag(&Hash(bytes)).ok()?;
                parse_version(&t.name).ok().map(|v| (v, t))
            })
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

    /// scope1: like `resolve_scope`, for reads. A view must have a record (it was written,
    /// proposed or landed), unless it is `own_view`, the reading session's view, which is
    /// created lazily. Reading a view that doesn't exist would silently read canon.
    pub fn resolve_read_scope(
        &self,
        text: &str,
        own_view: Option<&str>,
    ) -> Result<Scope, RdyError> {
        let scope = self.resolve_scope(text)?;
        if let Scope::View(v) = &scope
            && Some(v.as_str()) != own_view
            && self.store.get_ref(&format!("view/{v}"))?.is_none()
        {
            let hex = (7..=64).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_hexdigit());
            let fix = if hex {
                format!("{v:?} looks like a landing id: read it with landing:{v}")
            } else {
                "list views with rdy status (MCP: rdy_overview), or read canon".to_string()
            };
            return Err(RdyError::new(E_NOT_FOUND, format!("no view {v:?}"), fix));
        }
        Ok(scope)
    }

    /// Reads scope text: `canon`, `tag:<version>`, `landing:<id>`, `view:<name>`, or a view name.
    pub fn resolve_scope(&self, text: &str) -> Result<Scope, RdyError> {
        if text == "canon" {
            return Ok(Scope::Canon);
        }
        if let Some(name) = text.strip_prefix("tag:") {
            let want = parse_version(name)?;
            return self
                .version_tags()?
                .into_iter()
                .find(|(v, _)| v.cmp_precedence(&want) == Ordering::Equal)
                .map(|(_, t)| Scope::At(t.landing))
                .ok_or_else(|| {
                    RdyError::new(
                        E_NOT_FOUND,
                        format!("no version {name}"),
                        "list versions with rdy tags",
                    )
                });
        }
        if let Some(id) = text.strip_prefix("landing:") {
            return Ok(Scope::At(self.resolve_landing(id)?));
        }
        let view = text.strip_prefix("view:").unwrap_or(text);
        if view.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "a view name is empty",
                "name a view, e.g. view:work",
            ));
        }
        Ok(Scope::View(view.to_string()))
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
        let head = *chain.first().ok_or_else(|| {
            RdyError::new(
                E_NOT_FOUND,
                "canon has no landings to version",
                "land a proposal first",
            )
        })?;
        let target = match landing {
            Some(text) => match_prefix(&chain, text)?,
            None => head,
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

    /// The version position of `scope` (see `VersionPosition`).
    pub fn version_position(&self, scope: &Scope) -> Result<VersionPosition, RdyError> {
        let (base, view) = match scope {
            Scope::Canon => (self.store.canon_head()?, None),
            Scope::At(h) => (Some(*h), None),
            Scope::View(name) => {
                let v = self.store.view(name)?;
                let pos = ViewPosition {
                    name: name.clone(),
                    unlanded_changes: v.changes.len() as u32,
                };
                (v.base, Some(pos))
            }
        };
        // the highest version on each landing (version_tags is in ascending order)
        let mut on: HashMap<Hash, String> = HashMap::new();
        for (_, t) in self.version_tags()? {
            on.insert(t.landing, t.name);
        }
        let (mut since, mut cur, mut version) = (0u32, base, None);
        while let Some(h) = cur {
            if let Some(name) = on.get(&h) {
                version = Some(VersionRef {
                    name: name.clone(),
                    landing: h.to_string(),
                });
                break;
            }
            since += 1;
            cur = self.store.get::<LandingRecord>(&h)?.prev;
        }
        let frozen = matches!(scope, Scope::At(_));
        Ok(VersionPosition {
            label: label_of(&version, since, &view, frozen),
            version,
            landings_since: since,
            view,
            frozen,
        })
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
