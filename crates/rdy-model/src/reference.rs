//! The ref grammar (refv1): one line that cites a block, `[id] path: "snippet"` or
//! `[id@version] path: "snippet"`, and the parser the read tools use to take it back.
//! One implementation for the rendered site, the MCP tools and the CLI.

/// What a read call was pointed at: a block id or path, and the version it pins, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub id: String,
    pub version: Option<String>,
}

/// The fix hint every read surface gives with a malformed ref.
pub const BAD_REF_FIX: &str =
    "give an id, id@version, or a ref line such as [c-7KQ2M@v1.0.0] Doc: \"text\"";

/// The fix hint every read surface gives when a pinned version meets a different scope.
pub const PIN_CLASH_FIX: &str = "use the pinned version, or read the bare id with a scope";

fn bad(text: &str) -> String {
    format!("a ref is [id] or [id@version]: {text}")
}

/// Parses `id`, `id@version` or a whole ref line (the bracketed head is read, the rest is
/// ignored). The id part is left for `resolve_id` to judge. A proposal version `P-x@n` keeps
/// its own meaning and comes back whole as the id.
pub fn parse_target(text: &str) -> Result<Target, String> {
    let text = text.trim();
    let head = match text.strip_prefix('[') {
        Some(rest) => match rest.find(']') {
            Some(end) => &rest[..end],
            None => return Err(bad(text)),
        },
        None => text,
    };
    let head = head.trim();
    let (id, version) = match head.rfind('@') {
        Some(at) if !head.starts_with("P-") => (&head[..at], Some(&head[at + 1..])),
        _ => (head, None),
    };
    let id = id.trim();
    if id.is_empty() {
        return Err(bad(text));
    }
    let version = match version.map(str::trim) {
        Some("") => return Err(bad(text)),
        other => other,
    };
    Ok(Target {
        id: id.to_string(),
        version: version.map(str::to_string),
    })
}

impl Target {
    /// The scope text a read under this target uses: `Some("tag:<version>")` for a pin,
    /// `given` unchanged otherwise (the caller applies its own default). A `given` scope that
    /// differs from the pin is an error message, to be mapped to `E_BAD_INPUT` with
    /// [`PIN_CLASH_FIX`].
    pub fn scope_text(&self, given: Option<&str>) -> Result<Option<String>, String> {
        match &self.version {
            Some(v) => {
                let pin = format!("tag:{v}");
                if given.is_some_and(|s| s != pin) {
                    return Err(format!(
                        "{} pins version {v}; drop scope or make it tag:{v}",
                        self.id
                    ));
                }
                Ok(Some(pin))
            }
            None => Ok(given.map(str::to_string)),
        }
    }
}

/// The most characters of a repo link alias (repo links).
pub const MAX_ALIAS_CHARS: usize = 24;

/// A foreign reference token (repo links, D-DAPR8): `alias:id` or `alias:KEY` as an author
/// writes it, or `<repo>:<id>` as the store keeps it (32 lowercase hex digits, a colon, a block
/// id). Read from the token of a `{{…}}` reference or from a link target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForeignRef<'a> {
    /// An alias and the id or key it names in the alias's delivered content.
    Alias { alias: &'a str, target: &'a str },
    /// The stored form: the cited repo and the block id there.
    Stored {
        repo: crate::RepoId,
        id: crate::BlockId,
    },
}

/// Whether `s` is a repo link alias: `[a-z][a-z0-9-]{0,23}`.
pub fn is_alias(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=MAX_ALIAS_CHARS).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// The foreign reference `token` holds, if any: `<32 lowercase hex>:<block id>` is the stored
/// form; `<alias>:<id or key>` is the author's form. Any other token, a local id or key
/// included, is `None` (a key or id never holds a colon).
pub fn foreign_ref(token: &str) -> Option<ForeignRef<'_>> {
    let (left, right) = token.split_once(':')?;
    if left.len() == 32 && left.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
        return Some(ForeignRef::Stored {
            repo: left.parse().ok()?,
            id: crate::BlockId::parse(right).ok()?,
        });
    }
    let target_ok = crate::BlockId::is_valid(right) || crate::key::problem(right).is_none();
    (is_alias(left) && target_ok).then_some(ForeignRef::Alias {
        alias: left,
        target: right,
    })
}

/// The stored token of a foreign reference: `<repo>:<id>`.
pub fn foreign_token(repo: &crate::RepoId, id: &crate::BlockId) -> String {
    format!("{repo}:{id}")
}

/// The snippet of a ref: whitespace collapsed, a straight `"` turned into `'`, cut at the last
/// space before `max` characters with `…` appended; one token longer than `max` is cut
/// mid-token.
pub fn snippet(text: &str, max: usize) -> String {
    let flat = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('"', "'");
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= max {
        return flat;
    }
    let cut = if chars[max] == ' ' {
        max
    } else {
        chars[..max]
            .iter()
            .rposition(|c| *c == ' ')
            .filter(|i| *i > 0)
            .unwrap_or(max)
    };
    let kept: String = chars[..cut].iter().collect();
    format!("{}…", kept.trim_end())
}

fn title(t: &str) -> String {
    t.replace('"', "'").replace(['\r', '\n'], " ")
}

/// `[id]` or `[id@version]`, a space, then the path joined by ` › `.
pub fn head(id: &str, version: Option<&str>, path: &[&str]) -> String {
    let mut out = match version {
        Some(v) => format!("[{id}@{v}]"),
        None => format!("[{id}]"),
    };
    if !path.is_empty() {
        out.push(' ');
        out.push_str(
            &path
                .iter()
                .map(|t| title(t))
                .collect::<Vec<_>>()
                .join(" › "),
        );
    }
    out
}

/// The whole ref line: `{head}: "{snippet}"`.
pub fn format_ref(head: &str, snippet: &str) -> String {
    format!("{head}: \"{snippet}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refv1_parse_target_takes_ids_pins_and_whole_ref_lines() {
        assert_eq!(
            parse_target("c-WYF81").unwrap(),
            Target {
                id: "c-WYF81".into(),
                version: None
            }
        );
        assert_eq!(
            parse_target("c-WYF81@v1.2.0").unwrap(),
            Target {
                id: "c-WYF81".into(),
                version: Some("v1.2.0".into())
            }
        );
        assert_eq!(
            parse_target("[c-WYF81@v1.2.0] Household ledger › Components: \"The Balance…\"")
                .unwrap(),
            Target {
                id: "c-WYF81".into(),
                version: Some("v1.2.0".into())
            }
        );
        assert_eq!(
            parse_target("[c-WYF81] x: \"a@b\"").unwrap(),
            Target {
                id: "c-WYF81".into(),
                version: None
            }
        );
        assert_eq!(
            parse_target("wallets/transactions@1.2.0").unwrap(),
            Target {
                id: "wallets/transactions".into(),
                version: Some("1.2.0".into())
            }
        );
        assert_eq!(
            parse_target("P-7KQ2M@3").unwrap(),
            Target {
                id: "P-7KQ2M@3".into(),
                version: None
            }
        );
        assert!(
            parse_target("c-WYF81@")
                .unwrap_err()
                .starts_with("a ref is [id] or [id@version]")
        );
        assert!(parse_target("[]").is_err() && parse_target("@v1").is_err());
    }
    #[test]
    fn rpl12_foreign_ref_reads_alias_and_stored_forms() {
        let repo = "07".repeat(16);
        assert_eq!(
            foreign_ref(&format!("{repo}:d-FRGNX")),
            Some(ForeignRef::Stored {
                repo: crate::RepoId([7; 16]),
                id: crate::BlockId::parse("d-FRGNX").unwrap()
            })
        );
        assert_eq!(
            foreign_ref("core:d-FRGNX"),
            Some(ForeignRef::Alias {
                alias: "core",
                target: "d-FRGNX"
            })
        );
        assert_eq!(
            foreign_ref("core-2:CORE-RULE"),
            Some(ForeignRef::Alias {
                alias: "core-2",
                target: "CORE-RULE"
            })
        );
        // not foreign: no colon, a bad alias, a bad target, an upper-case or short repo
        let upper = format!("{}:d-FRGNX", "AB".repeat(16));
        let bad_id = format!("{repo}:not an id");
        let long = format!("{}:d-FRGNX", "a".repeat(25));
        for t in [
            "d-FRGNX",
            "KEY",
            "Core:d-FRGNX",
            "core:",
            "core:a b",
            upper.as_str(),
            "0707:d-FRGNX",
            bad_id.as_str(),
            long.as_str(),
        ] {
            assert_eq!(foreign_ref(t), None, "{t}");
        }
        assert_eq!(
            foreign_token(
                &crate::RepoId([7; 16]),
                &crate::BlockId::parse("d-FRGNX").unwrap()
            ),
            format!("{repo}:d-FRGNX")
        );
    }
    #[test]
    fn refv1_snippet_and_head_follow_the_grammar() {
        assert_eq!(
            snippet(
                "The Balance store keeps no balance column; every balance is derived from the ledger at read time, which is the whole point.",
                80
            ),
            "The Balance store keeps no balance column; every balance is derived from the…"
        );
        assert_eq!(snippet("say \"hi\"\nnow", 80), "say 'hi' now");
        assert_eq!(
            snippet(&"x".repeat(100), 80),
            format!("{}…", "x".repeat(80))
        );
        assert_eq!(
            head(
                "c-WYF81",
                Some("v1.2.0"),
                &["Household \"ledger\"", "Components"]
            ),
            "[c-WYF81@v1.2.0] Household 'ledger' › Components"
        );
        assert_eq!(
            format_ref(&head("c-WYF81", None, &["Doc"]), &snippet("Body.", 80)),
            "[c-WYF81] Doc: \"Body.\""
        );
    }
    #[test]
    fn refv1_scope_text_pins_clashes_and_passes_the_given_scope_through() {
        let bare = parse_target("c-WYF81").unwrap();
        assert_eq!(bare.scope_text(None), Ok(None));
        assert_eq!(bare.scope_text(Some("view:x")), Ok(Some("view:x".into())));
        let pinned = parse_target("[c-WYF81@v1.2.0] Doc: \"Body.\"").unwrap();
        assert_eq!(pinned.scope_text(None), Ok(Some("tag:v1.2.0".into())));
        assert_eq!(
            pinned.scope_text(Some("tag:v1.2.0")),
            Ok(Some("tag:v1.2.0".into()))
        );
        assert_eq!(
            pinned.scope_text(Some("canon")),
            Err("c-WYF81 pins version v1.2.0; drop scope or make it tag:v1.2.0".to_string())
        );
        assert!(BAD_REF_FIX.contains("id@version") && PIN_CLASH_FIX.contains("pinned version"));
    }
}
