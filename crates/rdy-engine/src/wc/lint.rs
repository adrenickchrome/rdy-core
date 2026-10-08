//! The working-copy lint (MS, FW-D10, R-PF1): the tree's own diagnostics, the per-entry lint
//! `rdy check` runs, and the split rules: supporting blocks, store-only links, pins and rdy's own
//! fields live only in the store, so a file that holds one is refused.

use crate::Materialized;
use crate::error::{E_WC_STORE_ONLY, E_WC_SUPPORTING};
use crate::lint::{LintItem, lint_blocks};
use crate::tree_io::Diagnostic;
use crate::wc::layout::{WcBlock, WcTree};
use crate::wc::plan::resolve_figures;
use rdy_model::BlockId;
use rdy_schema::{
    Pack, STORE_ONLY_FIELDS, STORE_ONLY_LINK_KINDS, STORE_ONLY_TARGET_TYPES, TypeClass,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Lints a working tree: every tree diagnostic, then the figure rule (with a `root`: a figure's
/// image path is checked and its `media_type` inferred in the tree, before the entries are
/// validated), then the per-entry lint (references are checked only against a `view`; an entry
/// that left out `order` is not checked for it), then the split rules. Sorted by file, line and
/// column.
pub fn lint_tree(
    tree: &mut WcTree,
    pack: &Pack,
    view: Option<&Materialized>,
    root: Option<&Path>,
) -> Vec<Diagnostic> {
    let mut diags = tree.diags.clone();
    if let Some(root) = root {
        diags.extend(resolve_figures(tree, root));
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
    diags.extend(lint_blocks(pack, view, &items, view.is_some()));
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
    for b in &tree.blocks {
        split_rules(b, pack, &store_only_target, &mut diags);
    }
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
        for l in ids.iter().filter(|l| store_only_target(&l.target)) {
            out.push(Diagnostic::error(
                &b.file,
                key("links"),
                E_WC_STORE_ONLY,
                format!(
                    "links to decisions and questions are store-only ({kind} -> {})",
                    l.target
                ),
                set_fix(kind, &l.target.to_string()),
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
