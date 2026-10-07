//! `rdy check`: lints format-2 files without touching the store (addendum B §4.1).
use crate::Materialized;
use crate::error::*;
use crate::io::KeyedLink;
use crate::keys::{Keys, key_of};
use crate::tree_io::{Diagnostic, read_file, read_tree};
use rdy_model::{Block, BlockId, FieldValue, RefMode, key, ref_spans};
use rdy_schema::{Pack, Severity, skeleton_gaps, validate_block, validate_prefix};
use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::path::Path;

fn field_tokens(v: &FieldValue, out: &mut Vec<String>) {
    match v {
        FieldValue::Str(s) => {
            out.extend(ref_spans(s, RefMode::Strict).into_iter().map(|s| s.token))
        }
        FieldValue::List(items) => items.iter().for_each(|i| field_tokens(i, out)),
        FieldValue::Map(m) => m.values().for_each(|i| field_tokens(i, out)),
        _ => {}
    }
}

/// One entry to lint: its block and where its file holds it.
pub(crate) struct LintItem<'a> {
    pub block: &'a Block,
    pub file: &'a str,
    pub at: (usize, usize),
    pub keys: &'a BTreeMap<String, (usize, usize)>,
    pub keyed: &'a [KeyedLink],
    /// The entry gave its `order`: false skips `E_ORDER_KEY` (a working-copy entry may leave it
    /// out; rdy assigns it).
    pub check_order: bool,
}

/// Where an issue from validate_block points inside its entry.
fn issue_at(l: &LintItem<'_>, code: &str, message: &str) -> (usize, usize) {
    let key = |k: &str| l.keys.get(k).copied();
    let found = match code {
        "E_UNKNOWN_TYPE" => key("type"),
        "E_PREFIX" => key("id"),
        "E_BODY_REQUIRED" | "E_BODY_FORBIDDEN" => key("body"),
        "E_LINK_TYPE" => key("links"),
        "E_CHILD_TYPE" | "E_ORPHAN" | "E_MISSING_PARENT" => key("parent"),
        "E_BAD_KEY" => key("fields.key"),
        "E_FIELD_UNKNOWN" | "E_FIELD_KIND" | "E_FIELD_ENUM" | "E_DOC_TYPE" | "E_BAD_REF" => l
            .keys
            .iter()
            .find(|(k, _)| {
                k.strip_prefix("fields.")
                    .is_some_and(|f| message.contains(&format!("{f:?}")))
            })
            .map(|(_, at)| *at)
            .or_else(|| key("body").filter(|_| code == "E_BAD_REF")),
        _ => None,
    };
    found.unwrap_or(l.at)
}

pub fn check_path(
    pack: &Pack,
    canon: Option<&Materialized>,
    path: &Path,
) -> Result<Vec<Diagnostic>, RdyError> {
    if !path.exists() {
        return Err(RdyError::new(
            E_NOT_FOUND,
            format!("{} does not exist", path.display()),
            "pass a doc file, a _folder.yaml or a directory",
        ));
    }
    let shown = path.display().to_string();
    let (loaded, mut diags) = if path.is_file() {
        read_file(path, &shown)
    } else if std::fs::symlink_metadata(path.join("rdy-export.yaml")).is_ok() {
        // sym1: the export's own manifest and docs root are never followed
        let mut diags = vec![];
        for name in ["rdy-export.yaml", "docs"] {
            let p = path.join(name);
            if crate::tree_io::is_symlink(&p) {
                diags.push(crate::tree_io::link_diag(&format!("{shown}/{name}")));
            }
        }
        if diags.is_empty() {
            read_tree(&path.join("docs"), &format!("{shown}/docs"))
        } else {
            (vec![], diags)
        }
    } else {
        read_tree(path, &shown)
    };
    let items: Vec<LintItem<'_>> = loaded
        .iter()
        .map(|l| LintItem {
            block: &l.block,
            file: &l.file,
            at: l.at,
            keys: &l.keys,
            keyed: &l.keyed,
            check_order: true,
        })
        .collect();
    diags.extend(lint_blocks(pack, canon, &items, true));
    if let Some(m) = canon {
        let nameless: Vec<&BlockId> = m
            .iter()
            .filter(|b| b.block_type == rdy_model::tree::DOC && b.field_str("name").is_none())
            .map(|b| &b.id)
            .collect();
        // one summary, not one warning per doc on every check
        if let Some(first) = nameless.first() {
            let n = nameless.len();
            let (docs, have) = if n == 1 {
                ("doc", "has")
            } else {
                ("docs", "have")
            };
            diags.push(Diagnostic {
                file: "canon".into(), line: 1, column: 1, code: "W_NAMELESS_DOC".into(),
                message: format!("{n} {docs} in canon {have} no stored name (from M1, e.g. {first}); the first export→import or write of each stores one"),
                fix: "write to each once (any edit) to store its name; rdy export lists them".into(),
                severity: Severity::Warning,
            });
        }
    }
    diags.sort_by(|a, b| (&a.file, a.line, a.column).cmp(&(&b.file, b.line, b.column)));
    Ok(diags)
}

/// The per-entry lint shared by `rdy check` and the working copy: duplicate ids, key clashes
/// (with the files and `view`), skeleton gaps, validation and, with `refs`, references that
/// neither the items nor `view` hold.
pub(crate) fn lint_blocks(
    pack: &Pack,
    view: Option<&Materialized>,
    items: &[LintItem<'_>],
    refs: bool,
) -> Vec<Diagnostic> {
    let mut diags = vec![];
    let mut first: BTreeMap<BlockId, &LintItem<'_>> = BTreeMap::new();
    for l in items {
        if let Some(other) = first.get(&l.block.id) {
            diags.push(Diagnostic::error(
                l.file,
                l.at,
                E_ALREADY_EXISTS,
                format!("block {} also appears in {}", l.block.id, other.file),
                "give one of them a new id, or remove the duplicate",
            ));
        } else {
            first.insert(l.block.id.clone(), l);
        }
    }
    let lookup = |id: &BlockId| -> Option<&Block> {
        first
            .get(id)
            .map(|l| l.block)
            .or_else(|| view.and_then(|m| m.get(id)))
    };
    // key1: keys of the checked files, and of canon blocks the files don't redefine (each indexed
    // only when a token or a key needs it)
    let file_index: OnceCell<Keys> = OnceCell::new();
    let file_keys = || file_index.get_or_init(|| Keys::of(first.values().map(|l| l.block)));
    let canon_index: OnceCell<Keys> = OnceCell::new();
    let canon_keys = || {
        canon_index.get_or_init(|| match view {
            Some(m) => Keys::of(m.iter().filter(|b| !first.contains_key(&b.id))),
            None => Keys::default(),
        })
    };
    let is_key = |tok: &str| {
        key::problem(tok).is_none()
            && (file_keys().get(tok).is_some() || canon_keys().get(tok).is_some())
    };
    // skel1: the section titles under each checked doc, from the checked files
    let section_titles = crate::keys::section_titles(first.values().map(|l| l.block));
    let mut keys_seen: BTreeMap<String, Vec<&BlockId>> = BTreeMap::new();
    for l in items {
        let b = l.block;
        let is_first = first.get(&b.id).is_some_and(|f| std::ptr::eq(*f, l));
        if is_first && let Some(k) = key_of(b) {
            let at = l.keys.get("fields.key").copied().unwrap_or(l.at);
            let earlier = keys_seen.entry(key::fold(k)).or_default();
            let canon_holders = canon_keys().holders(k);
            for other in earlier
                .iter()
                .copied()
                .chain(canon_holders.iter().map(|(_, id)| id))
                .filter(|o| *o != &b.id)
            {
                let e = crate::keys::clash_error(k, other, &b.id);
                diags.push(Diagnostic::error(l.file, at, e.code, e.message, e.fix));
            }
            earlier.push(&b.id);
        }
        if is_first && b.block_type == rdy_model::tree::DOC {
            let titles = section_titles
                .get(&b.id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let at = l.keys.get("fields.doc_type").copied().unwrap_or(l.at);
            for i in skeleton_gaps(pack, b, titles) {
                diags.push(Diagnostic {
                    file: l.file.to_string(),
                    line: at.0,
                    column: at.1,
                    code: i.code.into(),
                    message: i.message,
                    fix: i.fix,
                    severity: i.severity,
                });
            }
        }
        let parent = b.parent.as_ref().and_then(lookup);
        for i in validate_prefix(pack, b)
            .into_iter()
            .chain(validate_block(pack, b, parent))
            .filter(|i| l.check_order || i.code != "E_ORDER_KEY")
        {
            let at = issue_at(l, i.code, &i.message);
            diags.push(Diagnostic {
                file: l.file.to_string(),
                line: at.0,
                column: at.1,
                code: i.code.into(),
                message: i.message,
                fix: i.fix,
                severity: i.severity,
            });
        }
        if !refs {
            continue;
        }
        let unknown = |tok: &str| {
            BlockId::parse(tok)
                .ok()
                .and_then(|id| lookup(&id))
                .is_none()
                && !is_key(tok)
        };
        let mut refs: Vec<(String, String)> = vec![];
        for (k, v) in &b.fields {
            let mut toks = vec![];
            field_tokens(v, &mut toks);
            refs.extend(toks.into_iter().map(|t| (format!("fields.{k}"), t)));
        }
        refs.extend(
            ref_spans(&b.body, RefMode::for_body(&b.block_type))
                .into_iter()
                .map(|s| ("body".to_string(), s.token)),
        );
        refs.extend(
            b.links
                .values()
                .flatten()
                .map(|l| ("links".to_string(), l.target.to_string())),
        );
        refs.extend(
            l.keyed
                .iter()
                .map(|k| ("links".to_string(), k.text().to_string())),
        );
        for (key, tok) in refs {
            if unknown(&tok) {
                let at = l.keys.get(&key).copied().unwrap_or(l.at);
                // a {{token}} in a body or a string field says how to write it literally; a link
                // target keeps its text
                let (shown_tok, fix) = if key == "links" {
                    (
                        tok.clone(),
                        "fix the id, or check the files that define it together".to_string(),
                    )
                } else {
                    (
                        format!("{{{{{tok}}}}}"),
                        format!(
                            "fix the id, or check the files that define it together; {}",
                            crate::keys::literal_hint(&tok)
                        ),
                    )
                };
                diags.push(Diagnostic::error(
                    l.file,
                    at,
                    E_UNKNOWN_REF,
                    format!(
                        "{} references {shown_tok}, which is in neither the checked files nor canon",
                        b.id
                    ),
                    fix,
                ));
            }
        }
    }
    diags
}
