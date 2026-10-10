//! The working-copy lint (MS, FW-D10, R-PF1): the tree's own diagnostics, the per-entry lint
//! `rdy check` runs, and the split rules: supporting blocks, store-only links, pins and rdy's own
//! fields live only in the store, so a file that holds one is refused.

use crate::Materialized;
use crate::error::{E_VALIDATION, E_WC_STORE_ONLY, E_WC_SUPPORTING, RdyError};
use crate::keys::{Keys, resolve_text};
use crate::lint::{LintItem, lint_blocks};
use crate::tree_io::Diagnostic;
use crate::wc::layout::{WcBlock, WcTree};
use crate::wc::plan::resolve_figures;
use rdy_model::BlockId;
use rdy_schema::{
    Pack, STORE_ONLY_FIELDS, STORE_ONLY_LINK_KINDS, STORE_ONLY_TARGET_TYPES, TypeClass,
};
use serde_json::Value;
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Lints a working tree: every tree diagnostic, then the figure rule (with a `root`: a figure's
/// image path is checked and its `media_type` inferred in the tree, before the entries are
/// validated), then the split rules, then the per-entry lint (references are checked only against
/// a `view`; an entry that left out `order` is not checked for it). An entry of a supporting type
/// gets no `E_PREFIX`: the entry must leave the file, so another id is no fix. Sorted by file,
/// line and column; at one place the split rules come first.
pub fn lint_tree(
    tree: &mut WcTree,
    pack: &Pack,
    view: Option<&Materialized>,
    root: Option<&Path>,
) -> Vec<Diagnostic> {
    let mut diags = tree.diags.clone();
    if let Some(root) = root {
        diags.extend(resolve_figures(tree, root, pack));
    }
    let tree = &*tree;
    let items: Vec<LintItem<'_>> = tree
        .blocks
        .iter()
        .map(|b| LintItem {
            block: &b.block,
            file: &b.file,
            at: b.at,
            keys: &b.keys,
            keyed: &b.keyed,
            check_order: b.order_given,
        })
        .collect();
    let types: BTreeMap<&BlockId, &str> = tree
        .blocks
        .iter()
        .map(|b| (&b.block.id, b.block.block_type.as_str()))
        .collect();
    // a target in the tree has its type there; one outside it, the view's, else the type that
    // owns its id's prefix (R-P2)
    let store_only_target = |target: &BlockId| {
        let ty = match types.get(target) {
            Some(t) => Some(*t),
            None => view
                .and_then(|m| m.get(target))
                .map(|b| b.block_type.as_str())
                .or_else(|| pack.type_for_prefix(target.prefix())),
        };
        ty.is_some_and(|t| STORE_ONLY_TARGET_TYPES.contains(&t))
    };
    // a target written as a key: the tree's block with that key, else the view's (only with a
    // view; without one the commit's `wc_commit` checks it against the view)
    let tree_keys: OnceCell<Keys> = OnceCell::new();
    let view_keys: OnceCell<Keys> = OnceCell::new();
    let store_only_key = |text: &str| {
        let Some(m) = view else { return false };
        let in_tree = tree_keys
            .get_or_init(|| Keys::of(tree.blocks.iter().map(|b| &b.block)))
            .get(text)
            .map(|(_, id)| id.clone());
        let id = in_tree.or_else(|| {
            view_keys
                .get_or_init(|| Keys::of(m.iter().filter(|b| !types.contains_key(&b.id))))
                .get(text)
                .map(|(_, id)| id.clone())
        });
        id.is_some_and(|id| store_only_target(&id))
    };
    for b in &tree.blocks {
        split_rules(b, pack, &store_only_target, &store_only_key, &mut diags);
    }
    // where the E_PREFIX of a supporting entry would sit (its `id` key)
    let supporting: BTreeSet<(&str, (usize, usize))> = tree
        .blocks
        .iter()
        .filter(|b| pack.class_of(&b.block.block_type) == TypeClass::Supporting)
        .map(|b| (b.file.as_str(), b.keys.get("id").copied().unwrap_or(b.at)))
        .collect();
    diags.extend(
        lint_blocks(pack, view, &items, view.is_some())
            .into_iter()
            .filter(|d| {
                d.code != "E_PREFIX" || !supporting.contains(&(d.file.as_str(), (d.line, d.column)))
            }),
    );
    diags.sort_by(|a, b| (&a.file, a.line, a.column).cmp(&(&b.file, b.line, b.column)));
    diags
}

/// The fix of a store-only link.
fn set_fix(kind: &str, target: &str) -> String {
    format!("set it with rdy_write links, e.g. {{\"{kind}\": [\"{target}\"]}}; files never show it")
}

/// The split rules for one entry, each an error at the entry or at the key it names.
fn split_rules(
    b: &WcBlock,
    pack: &Pack,
    store_only_target: &dyn Fn(&BlockId) -> bool,
    store_only_key: &dyn Fn(&str) -> bool,
    out: &mut Vec<Diagnostic>,
) {
    let block = &b.block;
    let key = |k: &str| b.keys.get(k).copied().unwrap_or(b.at);
    if pack.class_of(&block.block_type) == TypeClass::Supporting {
        out.push(Diagnostic::error(
            &b.file,
            b.at,
            E_WC_SUPPORTING,
            format!(
                "{} is a {}, which lives only in the store",
                block.id, block.block_type
            ),
            "record it with rdy_decide (decisions and questions) or rdy_write, then remove the entry from the file",
        ));
    }
    let mut kinds: BTreeSet<&str> = block.links.keys().map(String::as_str).collect();
    kinds.extend(b.keyed.iter().map(|k| k.kind()));
    for kind in kinds {
        let ids = block.links.get(kind).map(Vec::as_slice).unwrap_or_default();
        if STORE_ONLY_LINK_KINDS.contains(&kind) {
            let first = ids
                .first()
                .map(|l| l.target.to_string())
                .or_else(|| {
                    b.keyed
                        .iter()
                        .find(|k| k.kind() == kind)
                        .map(|k| k.text().to_string())
                })
                .unwrap_or_else(|| "<target>".into());
            out.push(Diagnostic::error(
                &b.file,
                key("links"),
                E_WC_STORE_ONLY,
                format!("{kind} is a store-only link kind"),
                set_fix(kind, &first),
            ));
            continue;
        }
        let targets = ids
            .iter()
            .filter(|l| store_only_target(&l.target))
            .map(|l| l.target.to_string())
            .chain(
                b.keyed
                    .iter()
                    .filter(|k| k.kind() == kind && store_only_key(k.text()))
                    .map(|k| k.text().to_string()),
            );
        for target in targets {
            out.push(Diagnostic::error(
                &b.file,
                key("links"),
                E_WC_STORE_ONLY,
                format!("links to decisions and questions are store-only ({kind} -> {target})"),
                set_fix(kind, &target),
            ));
        }
    }
    if !block.pins.is_empty() {
        out.push(Diagnostic::error(
            &b.file,
            key("pins"),
            E_WC_STORE_ONLY,
            format!("pins are store-only ({})", block.id),
            "remove pins: rdy pins references itself",
        ));
    }
    for f in STORE_ONLY_FIELDS {
        if block.fields.contains_key(*f) {
            out.push(Diagnostic::error(
                &b.file,
                key(&format!("fields.{f}")),
                E_WC_STORE_ONLY,
                format!("{f} is set by rdy and lives only in the store"),
                format!("remove {f} from the file"),
            ));
        }
    }
}

/// The links of a working-copy commit's `ops` whose target is written as a key (or a path) and
/// names a decision or a question in `m` (the bound view): store-only links the client could not
/// see, so its lint let them through (amendment §5.2). `E_VALIDATION`, one `E_WC_STORE_ONLY`
/// issue per link at the op's block, or `Ok` when there is none. A target written as an id was
/// checked by the client; a target that resolves to nothing is left to the write.
pub fn check_keyed_targets(m: &Materialized, ops: &[Value]) -> Result<(), RdyError> {
    let mut issues = vec![];
    for op in ops {
        let Some(links) = op.get("links").and_then(Value::as_object) else {
            continue;
        };
        let block = op
            .get("id")
            .and_then(Value::as_str)
            .and_then(|i| BlockId::parse(i).ok());
        for (kind, list) in links {
            let entries: Vec<&Value> = match list {
                Value::Array(a) => a.iter().collect(),
                v => vec![v],
            };
            for e in entries {
                let Some(text) = e
                    .as_str()
                    .or_else(|| e.get("target").and_then(Value::as_str))
                else {
                    continue;
                };
                if BlockId::parse(text).is_ok() {
                    continue;
                }
                let Ok(id) = resolve_text(m, text) else {
                    continue;
                };
                if m.get(&id)
                    .is_some_and(|t| STORE_ONLY_TARGET_TYPES.contains(&t.block_type.as_str()))
                {
                    issues.push(rdy_schema::Issue {
                        severity: rdy_schema::Severity::Error,
                        code: E_WC_STORE_ONLY,
                        block: block.clone(),
                        message: format!(
                            "links to decisions and questions are store-only ({kind} -> {text})"
                        ),
                        fix: set_fix(kind, text),
                    });
                }
            }
        }
    }
    let Some(first) = issues.first() else {
        return Ok(());
    };
    let mut e = RdyError::new(E_VALIDATION, first.message.clone(), first.fix.clone());
    e.issues = issues;
    Err(e)
}
