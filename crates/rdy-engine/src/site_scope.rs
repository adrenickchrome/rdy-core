//! The scopes the live site serves and their paths (D-N5NSN, D-FV6G9): one place that spells a
//! scope as a path of the site, for the site's guard (which reads a path back) and for
//! `rdy_site` (which writes the link). One spelling per scope, so one build per scope.

/// A scope the site serves: canon at the root, a view under `~view/`, a proposal version under
/// `~proposal/`, a tag under `~tag/`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScopeRef {
    Canon,
    View(String),
    /// `P-<lowercase hex>[@<n>]`, as [`proposal_ref`] spells it.
    Proposal(String),
    /// A tag, by the name it is stored under: the frozen site of one version.
    Tag(String),
    /// The live site's versions page: not a read scope.
    Versions,
}

/// The change summary of a view or proposal, under the scope's prefix.
pub const CHANGES_PAGE: &str = "changes.html";
/// The change summary of canon since its current tag, at the site's root.
pub const SINCE_PAGE: &str = "changes-since.html";

impl ScopeRef {
    /// The scope as rdy reads it: `canon`, `view:<name>` or `proposal:<ref>`.
    pub fn text(&self) -> String {
        match self {
            ScopeRef::Canon => "canon".to_string(),
            ScopeRef::View(name) => format!("view:{name}"),
            ScopeRef::Proposal(r) => format!("proposal:{r}"),
            ScopeRef::Tag(name) => format!("tag:{name}"),
            ScopeRef::Versions => "versions".to_string(),
        }
    }

    /// Where the scope's pages start, under the site's root: empty for canon.
    pub fn path_prefix(&self) -> String {
        match self {
            ScopeRef::Canon => String::new(),
            ScopeRef::View(name) => format!("~view/{}/", percent_encode(name)),
            // the one spelling holds only `P`, `-`, hex digits and `@`: safe in a path as it is
            ScopeRef::Proposal(r) => format!("~proposal/{r}/"),
            // a version name holds only `0-9 A-Z a-z . -`, each valid as it is in a path segment,
            // and is never a step (`.` or `..`): written literally (ruling task-2)
            ScopeRef::Tag(name) => {
                debug_assert!(
                    tag_name_ok(name),
                    "a tag path holds a version name: {name:?}"
                );
                format!("~tag/{name}/")
            }
            ScopeRef::Versions => "~versions/".to_string(),
        }
    }
}

/// A view name as the engine allows it: non-empty, with no `:` (p-SBS6W). It may hold `/`, `.`
/// or `\`: on the site it is one encoded segment, never a path.
pub fn view_name_ok(name: &str) -> bool {
    !name.is_empty() && !name.contains(':')
}

/// A tag name as the site reads it: exactly what `parse_version` accepts, with no length cap of
/// its own (every version rdy stores has a site; ruling task-2). A SemVer name holds only
/// `0-9 A-Z a-z . -` here (build metadata is refused), never starts with `.` or `-`, and is
/// never a path step, so it is one path segment as it is. The one bound on its length is
/// `MAX_VERSION_BYTES`, which `parse_version` enforces; the request head's limit
/// (`MAX_HEAD_BYTES` in rdy-daemon) is asserted to carry the longest name at compile time.
pub fn tag_name_ok(name: &str) -> bool {
    crate::version::parse_version(name).is_ok()
}

/// The one spelling of a proposal ref: `P-` and 4 to 64 hex digits (either case), then optionally
/// `@` and a whole number from 1 (the forms `resolve_in` reads), as `P-<lowercase hex>[@<n>]`
/// with `n` free of a sign and of leading zeros. `None` for any other text. One version so has
/// one [`ScopeRef`], and so one build.
pub fn proposal_ref(r: &str) -> Option<String> {
    let rest = r.strip_prefix("P-")?;
    let (prefix, n) = match rest.split_once('@') {
        Some((prefix, n)) => (prefix, Some(n)),
        None => (rest, None),
    };
    if !(4..=64).contains(&prefix.len()) || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let prefix = prefix.to_ascii_lowercase();
    let Some(n) = n else {
        return Some(format!("P-{prefix}"));
    };
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = n.parse::<u32>().ok().filter(|n| *n >= 1)?;
    Some(format!("P-{prefix}@{n}"))
}

/// `s` with every byte outside `A-Z a-z 0-9 - _ ~` percent-encoded. `.` is encoded too, so a
/// view named `.` or `..` stays one segment for the site (ruling task-3).
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
