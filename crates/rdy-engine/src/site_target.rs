//! What a live-site listener serves (D-CWC91, D-DK03K): a source scope, one publication profile
//! of a scope, the profile-only site of canon, or the versions menu with its context. One small
//! enum, so a cache keyed by it cannot confuse one mode or scope with another, and one place
//! that spells each target as a path, a menu link and an event query.

use crate::publication::IMPLICIT_ORG;
use crate::site_scope::{ScopeRef, percent_encode, proposal_ref, tag_name_ok, view_name_ok};
use crate::{E_BAD_INPUT, RdyError};

/// The route segment that names a profile under a scope's prefix.
const PROFILE_SEGMENT: &str = "~profile/";
/// The menu page, at the site's root.
const MENU_PATH: &str = "/~versions/";

/// One thing a listener can serve.
///
/// A profile key is a stored valid key folded to ASCII lowercase (`rdy_model::key::fold`), or the
/// exact token [`IMPLICIT_ORG`]. A request that names a profile by id is resolved to its key
/// before a target is made. `Source(Versions)` is the menu of canon: it reads as
/// `Menu { scope: Canon, profile: None }` everywhere here, and the cache normalizes it so.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum SiteTarget {
    /// The whole source of a scope: the site as it is without a profile.
    Source(ScopeRef),
    /// The pages of one profile, under a scope: `<scope prefix>~profile/<key>/`.
    Profile { scope: ScopeRef, key: String },
    /// A listener that serves one profile of canon at its root, and nothing else. Only the
    /// listener sets this mode; no request value creates it.
    ProfileOnly { key: String },
    /// The versions menu as seen from a scope, optionally for one profile.
    Menu {
        scope: ScopeRef,
        profile: Option<String>,
    },
}

impl SiteTarget {
    /// The path the target's pages start at, with a leading and a closing `/`: `/` for source
    /// canon and for only-mode, the scope's prefix for another source target, and that prefix
    /// plus `~profile/<one encoded key>/` for a profile. A menu is served at `/~versions/`, for
    /// every scope and profile it is seen from.
    ///
    /// The key is encoded with the site's byte encoder, so it stays one path segment (a `.` and a
    /// `:` are encoded). The HTTP layer decodes a segment once.
    pub fn root(&self) -> String {
        match self {
            SiteTarget::Source(scope) => format!("/{}", scope.path_prefix()),
            SiteTarget::Profile { scope, key } => {
                format!(
                    "/{}{PROFILE_SEGMENT}{}/",
                    scope.path_prefix(),
                    percent_encode(key)
                )
            }
            SiteTarget::ProfileOnly { .. } => "/".to_string(),
            SiteTarget::Menu { .. } => MENU_PATH.to_string(),
        }
    }

    /// The scope the target reads: always canon for only-mode.
    pub fn scope(&self) -> ScopeRef {
        match self {
            SiteTarget::Source(scope)
            | SiteTarget::Profile { scope, .. }
            | SiteTarget::Menu { scope, .. } => scope.clone(),
            SiteTarget::ProfileOnly { .. } => ScopeRef::Canon,
        }
    }

    /// The link to the menu from the target's pages; `None` for only-mode, which has no menu.
    /// Canon without a profile (and the menu of canon) is the bare `/~versions/`; any other
    /// target adds `?scope=<scope>` and `&profile=<key>` when a profile is selected. Values hold
    /// only ASCII letters, digits and `- _ . ~` as they are; every other byte is `%XX`.
    pub fn menu_url(&self) -> Option<String> {
        let (scope, profile) = match self {
            SiteTarget::ProfileOnly { .. } => return None,
            SiteTarget::Source(scope) => (scope, None),
            SiteTarget::Profile { scope, key } => (scope, Some(key.as_str())),
            SiteTarget::Menu { scope, profile } => (scope, profile.as_deref()),
        };
        if profile.is_none() && matches!(scope, ScopeRef::Canon | ScopeRef::Versions) {
            return Some(MENU_PATH.to_string());
        }
        let mut url = format!("{MENU_PATH}?scope={}", encode(&scope.text(), is_unreserved));
        if let Some(key) = profile {
            url.push_str("&profile=");
            url.push_str(&encode(key, is_unreserved));
        }
        Some(url)
    }

    /// The query a page's live-update connection sends, without page, revision and label (the
    /// caller appends those, in that order): `scope=<scope>`, then `&profile=<key>`, then
    /// `&menu=1` for a menu. Only-mode sends `scope=canon`: the listener, not the client, holds
    /// its mode. The menu of canon keeps the base spelling `scope=versions`, with no `menu=1`,
    /// as its link keeps the bare `/~versions/`.
    ///
    /// Values keep `A-Z a-z 0-9 - . _ ~ : @` as they are and write every other byte as `%XX`.
    pub fn events_query(&self) -> String {
        match self {
            SiteTarget::ProfileOnly { .. } => "scope=canon".to_string(),
            SiteTarget::Source(ScopeRef::Versions)
            | SiteTarget::Menu {
                scope: ScopeRef::Canon,
                profile: None,
            } => "scope=versions".to_string(),
            SiteTarget::Source(scope) => format!("scope={}", encode(&scope.text(), is_event_safe)),
            SiteTarget::Profile { scope, key } => format!(
                "scope={}&profile={}",
                encode(&scope.text(), is_event_safe),
                encode(key, is_event_safe)
            ),
            SiteTarget::Menu { scope, profile } => {
                let mut q = format!("scope={}", encode(&scope.text(), is_event_safe));
                if let Some(key) = profile {
                    q.push_str("&profile=");
                    q.push_str(&encode(key, is_event_safe));
                }
                q.push_str("&menu=1");
                q
            }
        }
    }

    /// Whether the target is one the site can serve: every scope has its one spelling, no scope
    /// nested in a profile or a menu is `Versions`, and every key is a valid folded key or
    /// [`IMPLICIT_ORG`]. `Source(Versions)` is valid: it is the menu of canon.
    pub fn validate(&self) -> Result<(), RdyError> {
        let ok = match self {
            SiteTarget::Source(scope) => scope_ok(scope),
            SiteTarget::Profile { scope, key } => nested_scope_ok(scope) && key_ok(key),
            SiteTarget::ProfileOnly { key } => key_ok(key),
            SiteTarget::Menu { scope, profile } => {
                nested_scope_ok(scope) && profile.as_deref().is_none_or(key_ok)
            }
        };
        if ok {
            Ok(())
        } else {
            Err(RdyError::new(
                E_BAD_INPUT,
                "invalid live site target",
                "use a source scope and a publication profile key",
            ))
        }
    }
}

/// A scope in its one spelling, as the site's guard reads it.
fn scope_ok(scope: &ScopeRef) -> bool {
    match scope {
        ScopeRef::Canon | ScopeRef::Versions => true,
        ScopeRef::View(name) => view_name_ok(name),
        ScopeRef::Proposal(r) => proposal_ref(r).is_some_and(|one| one == *r),
        ScopeRef::Tag(name) => tag_name_ok(name),
    }
}

/// A scope under a profile or a menu: a source scope, never the versions page.
fn nested_scope_ok(scope: &ScopeRef) -> bool {
    !matches!(scope, ScopeRef::Versions) && scope_ok(scope)
}

/// A stored valid key in its folded form, or the exact implicit token.
fn key_ok(key: &str) -> bool {
    key == IMPLICIT_ORG
        || (rdy_model::key::problem(key).is_none() && rdy_model::key::fold(key) == key)
}

/// The bytes a menu link leaves as they are.
fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~')
}

/// The bytes an event query leaves as they are (the rule of the live renderer's encoder).
fn is_event_safe(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~:@".contains(&b)
}

/// `s` with every byte `keep` refuses as uppercase `%XX`.
fn encode(s: &str, keep: fn(u8) -> bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if keep(b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
