//! Briefing a code path (cbrief1, FW-C6, FW-L2): the blocks the file's commits cite in their
//! `Implements:` trailers, the blocks whose `reflects` globs match it, and an ordinary briefing
//! over both (each with every block under it). Decisions carry how many commits depend on them,
//! and whether the code citing them is provisional (still proposed) or due for rework (rejected).
//! Counts are reported as they are; nothing here ranks or cuts (FW-C6).
use crate::brief::MAX_BRIEF_BLOCKS;
use crate::code_index::{CodeIndex, CodeRepo, code_repos, index_of};
use crate::error::*;
use crate::{Briefing, ReadState, Scope};
use rdy_graph::DerivedStatus;
use rdy_model::{Block, BlockId, FieldValue, key, tree};
use rdy_schema::{Glob, REFLECTS_FIELD};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

/// The longest code path accepted, in bytes.
pub const MAX_CODE_PATH: usize = 4096;

/// A briefing over a code path: the ordinary briefing, and what linked the path to its blocks.
#[derive(Clone, Debug, Serialize)]
pub struct CodeBriefing {
    #[serde(flatten)]
    pub briefing: Briefing,
    pub code: CodeSection,
}

/// How a code path reached its blocks. Never cut by the briefing's size budget.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CodeSection {
    /// The path, relative to its repo's top folder.
    pub path: String,
    /// The repos consulted, as configured.
    pub repos: Vec<String>,
    /// The blocks the path's commits cite, in id order.
    pub implements: Vec<CodeAnchor>,
    /// The blocks whose `reflects` globs match the path, by (block, glob).
    pub reflects: Vec<ReflectsAnchor>,
    /// Trailer values that name no block in the scope, sorted.
    pub unresolved: Vec<String>,
    /// Some cited decision is still proposed.
    pub provisional: bool,
    /// Some cited decision is rejected.
    pub rework: bool,
}

/// One block a path's commits cite.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CodeAnchor {
    pub block: String,
    /// Distinct commits touching the path that cite the block.
    pub commits: usize,
    /// A decision's derived status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// A decision's distinct citing commits anywhere in the consulted repos.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependent_commits: Option<usize>,
    pub provisional: bool,
    pub rework: bool,
}

/// A block whose `reflects` glob matches the path.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReflectsAnchor {
    pub block: String,
    pub glob: String,
}

/// The code that cites one block.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CodeDependents {
    /// Distinct commits citing the block, whatever files they touched.
    pub commits: usize,
    /// The files at HEAD those commits left cited, by (repo, path).
    pub files: Vec<CodeFile>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CodeFile {
    pub repo: String,
    pub path: String,
}

/// `code_path` given with `ids` or `path`.
pub fn e_code_path_alone() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "code_path stands alone: not with ids or path",
        "call rdy_brief once with code_path, and again with ids or path",
    )
}

fn not_a_code_path(code_path: &str, fix: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("{code_path:?} is not a code path"),
        fix,
    )
}

/// `code_path` as (the one repo it points into, if its form says so; its path in that repo).
fn normalise(
    repos: &[CodeRepo],
    code_path: &str,
    cwd: Option<&Path>,
) -> Result<(Option<usize>, String), RdyError> {
    let top_fix = "give a file path relative to the code repo's top folder, such as src/main.rs";
    if code_path.is_empty() || code_path.len() > MAX_CODE_PATH || code_path.contains('\0') {
        return Err(not_a_code_path(code_path, top_fix));
    }
    let given = Path::new(code_path);
    let local = cwd.map(|c| c.join(given)).filter(|p| p.exists());
    if given.is_absolute() || local.is_some() {
        if repos.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "no code repo is linked",
                "add code_repos: [<path to the repo>] to .rdy/config.yaml",
            ));
        }
        let full = local.unwrap_or_else(|| given.to_path_buf());
        let p = std::fs::canonicalize(&full)
            .or_else(|_| {
                let name = full.file_name().ok_or(std::io::ErrorKind::NotFound)?;
                let parent = full.parent().ok_or(std::io::ErrorKind::NotFound)?;
                Ok::<_, std::io::Error>(std::fs::canonicalize(parent)?.join(name))
            })
            .unwrap_or(full);
        for (i, repo) in repos.iter().enumerate() {
            if let Ok(rest) = p.strip_prefix(&repo.root) {
                // Normal components only: a path that did not canonicalise may still hold `..`.
                let parts: Option<Vec<&str>> = rest
                    .components()
                    .map(|c| match c {
                        std::path::Component::Normal(s) => s.to_str(),
                        _ => None,
                    })
                    .collect();
                return match parts {
                    Some(parts) if !parts.is_empty() => Ok((Some(i), parts.join("/"))),
                    _ => Err(not_a_code_path(code_path, top_fix)),
                };
            }
        }
        let configured: Vec<&str> = repos.iter().map(|r| r.configured.as_str()).collect();
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{code_path} is not inside a linked code repo"),
            format!(
                "give a path inside one of: {}, or a path relative to a repo's top folder",
                configured.join(", ")
            ),
        ));
    }
    let mut rest = code_path;
    while let Some(r) = rest.strip_prefix("./") {
        rest = r;
    }
    if rest
        .split('/')
        .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Err(not_a_code_path(
            code_path,
            "a code path is relative to the repo's top folder, with no . or .. segments",
        ));
    }
    Ok((None, rest.to_string()))
}

/// The three ways a cited text reaches a block, compared directly (a key is compared ignoring
/// case): its id, its key, its path.
struct Names {
    id: String,
    key: Option<String>,
    path: Option<String>,
}

impl Names {
    fn of(m: &crate::Materialized, id: &BlockId) -> Names {
        Names {
            id: id.to_string(),
            key: m.get(id).and_then(|b| b.field_str("key")).map(key::fold),
            // Only a doc or folder has a path of its own; `path_of` gives any other block its
            // doc's path, which `resolve_id` would resolve to the doc.
            path: m
                .get(id)
                .filter(|b| tree::is_tree_type(&b.block_type))
                .and_then(|_| m.path_of(id)),
        }
    }

    fn names(&self, text: &str) -> bool {
        text == self.id
            || self.path.as_deref() == Some(text)
            || self.key.as_ref().is_some_and(|k| key::fold(text) == *k)
    }
}

/// The distinct commits in `indexes` citing a text that names the block.
fn dependent_hashes<'a>(
    indexes: impl IntoIterator<Item = &'a Arc<CodeIndex>>,
    names: &Names,
) -> BTreeSet<&'a str> {
    let mut hashes = BTreeSet::new();
    for idx in indexes {
        for (text, commits) in &idx.cited {
            if names.names(text) {
                hashes.extend(commits.iter().map(String::as_str));
            }
        }
    }
    hashes
}

/// The `reflects` globs of `b`, as written.
fn reflects_of(b: &Block) -> impl Iterator<Item = &str> {
    let items: &[FieldValue] = match b.fields.get(REFLECTS_FIELD) {
        Some(FieldValue::List(items)) => items,
        _ => &[],
    };
    items.iter().filter_map(FieldValue::as_str)
}

impl ReadState {
    /// Briefs the blocks `code_path` (a file in a linked code repo) is tied to in `scope`. Nothing
    /// is registered (see `Briefing::register`).
    pub fn brief_code(
        &self,
        scope: &Scope,
        code_path: &str,
        cwd: Option<&Path>,
        max_tokens: Option<usize>,
    ) -> Result<CodeBriefing, RdyError> {
        let repos = code_repos(self.root())?;
        let (which, path) = normalise(&repos, code_path, cwd)?;
        let consulted: Vec<&CodeRepo> = match which {
            Some(i) => vec![&repos[i]],
            None => repos.iter().collect(),
        };
        let mut indexes = Vec::with_capacity(consulted.len());
        let mut per_text: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for repo in &consulted {
            let idx = index_of(repo)?;
            if let Some(cited) = idx.files.get(&path) {
                for (text, hashes) in cited {
                    per_text
                        .entry(text.clone())
                        .or_default()
                        .extend(hashes.iter().cloned());
                }
            }
            indexes.push(idx);
        }

        let m = self.materialized(scope)?;
        let g = self.graph(scope)?;
        let mut unresolved = BTreeSet::new();
        let mut by_id: BTreeMap<BlockId, BTreeSet<String>> = BTreeMap::new();
        for (text, hashes) in per_text {
            match self.resolve_id(scope, &text) {
                Ok(id) if m.get(&id).is_some() => by_id.entry(id).or_default().extend(hashes),
                _ => {
                    unresolved.insert(text);
                }
            }
        }

        let mut implements = Vec::with_capacity(by_id.len());
        for (id, hashes) in &by_id {
            let status = g.status(id);
            let dependent_commits = status
                .as_ref()
                .map(|_| dependent_hashes(&indexes, &Names::of(&m, id)).len());
            implements.push(CodeAnchor {
                block: id.to_string(),
                commits: hashes.len(),
                status: status.as_ref().map(|s| s.tag().to_string()),
                dependent_commits,
                provisional: status == Some(DerivedStatus::Proposed),
                rework: status == Some(DerivedStatus::Rejected),
            });
        }

        let mut reflects = Vec::new();
        let mut reflecting = BTreeSet::new();
        for b in m.iter() {
            for glob in reflects_of(b) {
                if Glob::compile(glob).is_ok_and(|g| g.matches(&path)) {
                    reflects.push(ReflectsAnchor {
                        block: b.id.to_string(),
                        glob: glob.to_string(),
                    });
                    reflecting.insert(b.id.clone());
                }
            }
        }
        reflects.sort_by(|a, b| (&a.block, &a.glob).cmp(&(&b.block, &b.glob)));

        let roots: Vec<&BlockId> = by_id.keys().chain(reflecting.iter()).collect();
        let covered = with_descendants(&m, roots)?;
        let briefing = self.brief_ids(scope, &covered, max_tokens)?;

        let section = CodeSection {
            path,
            repos: consulted.iter().map(|r| r.configured.clone()).collect(),
            provisional: implements.iter().any(|a| a.provisional),
            rework: implements.iter().any(|a| a.rework),
            implements,
            reflects,
            unresolved: unresolved.into_iter().collect(),
        };
        Ok(CodeBriefing {
            briefing,
            code: section,
        })
    }

    /// The code that cites `id` in `scope`: how many commits, and the files at HEAD they left it
    /// on. One pass over each linked repo's citations; nothing is registered.
    pub fn code_dependents(&self, scope: &Scope, id: &BlockId) -> Result<CodeDependents, RdyError> {
        let m = self.materialized(scope)?;
        if m.get(id).is_none() {
            return Err(RdyError::new(
                E_UNKNOWN_BLOCK,
                format!("no block {id} in {}", scope_label(scope)),
                "check the id with rdy_read",
            ));
        }
        let repos = code_repos(self.root())?;
        let names = Names::of(&m, id);
        let mut indexes = Vec::with_capacity(repos.len());
        let mut files = Vec::new();
        for repo in &repos {
            let idx = index_of(repo)?;
            for (path, cited) in &idx.files {
                if cited.keys().any(|t| names.names(t)) {
                    files.push(CodeFile {
                        repo: repo.configured.clone(),
                        path: path.clone(),
                    });
                }
            }
            indexes.push(idx);
        }
        files.sort_by(|a, b| (&a.repo, &a.path).cmp(&(&b.repo, &b.path)));
        Ok(CodeDependents {
            commits: dependent_hashes(&indexes, &names).len(),
            files,
        })
    }
}

fn scope_label(scope: &Scope) -> String {
    match scope {
        Scope::Canon => "canon".to_string(),
        Scope::View(v) => format!("view {v}"),
        Scope::At(h) => format!("landing:{h}"),
    }
}

/// `roots` and every block under each, refused as soon as the set passes `MAX_BRIEF_BLOCKS`. A
/// block already seen is not walked again (a damaged store may loop).
fn with_descendants(
    m: &crate::Materialized,
    roots: Vec<&BlockId>,
) -> Result<BTreeSet<BlockId>, RdyError> {
    let too_many = || {
        RdyError::new(
            E_LIMIT,
            format!(
                "the code briefing covers more than {MAX_BRIEF_BLOCKS} blocks; the limit is {MAX_BRIEF_BLOCKS}"
            ),
            "brief the blocks you will change with ids instead",
        )
    };
    let mut seen: BTreeSet<BlockId> = BTreeSet::new();
    for root in roots {
        if !seen.insert(root.clone()) {
            continue;
        }
        let mut todo = vec![root.clone()];
        while let Some(cur) = todo.pop() {
            if seen.len() > MAX_BRIEF_BLOCKS {
                return Err(too_many());
            }
            for c in m.children(Some(&cur)) {
                if seen.insert(c.id.clone()) {
                    todo.push(c.id.clone());
                    if seen.len() > MAX_BRIEF_BLOCKS {
                        return Err(too_many());
                    }
                }
            }
        }
    }
    if seen.len() > MAX_BRIEF_BLOCKS {
        return Err(too_many());
    }
    Ok(seen)
}
