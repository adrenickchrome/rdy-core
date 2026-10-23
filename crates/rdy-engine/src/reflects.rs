//! Declared-reflects code state (D-01QX5; agents/design-and-code p-8EET1, p-6XMAK, p-EC3VW): what
//! a block's `reflects` globs matched in the linked checkouts when rdy last recorded it, and the
//! read-time comparison with what they match now.
//!
//! The record is the engine field `reflects_at` (p-HF5W0): a hash of the matched committed
//! files and each linked repo's HEAD. A write records it when it creates a reflects block, when
//! it changes the block's glob set, and on a repin with `code: true`; any other rewrite keeps the
//! old one. A code suspect is never stored: [`code_suspects`] compares committed tree identities
//! when it is read, so worktree and staged bytes, commit clocks and unrelated commits play no
//! part, and nothing enters the graph, a TRANSITION or a change plan. No watcher, no fetch.
use crate::code_index::{CodeRepo, check_top, code_repos, commit_exists, head_of, read_tree};
use crate::{E_CONFIG, Materialized, RdyError, ReadState, Scope};
use rdy_model::engine_fields::{REFLECTS_AT, ReflectsAt, decode_reflects_at, encode_reflects_at};
use rdy_model::plan::PlanHead;
use rdy_model::{Block, BlockId, FieldValue, Hash, Op};
use rdy_schema::{Glob, Issue, REFLECTS_FIELD, Severity};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

/// The warning of a write of a reflects block with no linked checkout.
pub const W_REFLECTS_NO_CODE: &str = "W_REFLECTS_NO_CODE";

/// The fix of [`W_REFLECTS_NO_CODE`].
pub const NO_CODE_FIX: &str = "set code_paths, then repin it with code: true";

/// The domain of the `files` hash.
const FILES_DOMAIN: &[u8] = b"rdy-reflects-files\0";

/// The code state of a block's reflected files: the hash of every matched committed file and
/// each linked repo's HEAD, in repo order (`None` for an unborn one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflectsSnapshot {
    /// BLAKE3 of `b"rdy-reflects-files\0"`, then each matched `(repo, path, blob)` row sorted by
    /// repo then path, each string as its u64 little-endian byte length and its UTF-8 bytes.
    pub files: Hash,
    pub commits: Vec<PlanHead>,
}

impl ReflectsSnapshot {
    /// The stored `reflects_at` value.
    fn to_field(&self) -> FieldValue {
        encode_reflects_at(&ReflectsAt {
            files: self.files,
            commits: self
                .commits
                .iter()
                .map(|c| (c.repo.clone(), c.head.clone()))
                .collect(),
        })
    }
}

/// A block whose reflected files differ from its recorded snapshot. `paths` are the changed
/// matched files on either side, as `<repo>:<path>`, sorted; empty for a repo whose recorded
/// commit this checkout no longer holds. A read projection, never in the graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeSuspect {
    pub block: BlockId,
    pub paths: Vec<String>,
}

/// `b`'s `reflects` globs as a set: sorted, each once, as written. Empty: it declares none.
/// Sorting and deduplicating here never rewrites the author's field.
pub(crate) fn glob_set(b: &Block) -> BTreeSet<&str> {
    match b.fields.get(REFLECTS_FIELD) {
        Some(FieldValue::List(items)) => items.iter().filter_map(FieldValue::as_str).collect(),
        _ => BTreeSet::new(),
    }
}

/// The globs of `set` that compile (validation refuses the others on every write).
fn compiled<'a>(set: impl IntoIterator<Item = &'a str>) -> Vec<Glob> {
    set.into_iter()
        .filter_map(|g| Glob::compile(g).ok())
        .collect()
}

/// The linked checkouts of the project at `root`, sorted by name, each checked as the code index
/// checks it: a configured checkout that is no usable repository keeps its `E_CONFIG`. Empty
/// when none is linked.
fn linked(root: &Path) -> Result<Vec<CodeRepo>, RdyError> {
    let mut repos = code_repos(root)?;
    repos.sort_by(|a, b| a.configured.cmp(&b.configured));
    for r in &repos {
        check_top(r)?;
    }
    Ok(repos)
}

/// One linked checkout at its HEAD: path → blob of each committed file.
struct Tree {
    repo: CodeRepo,
    head: Option<String>,
    blobs: BTreeMap<String, String>,
}

/// `repos` as their HEADs commit them; the worktree and the index play no part.
fn trees(repos: &[CodeRepo]) -> Result<Vec<Tree>, RdyError> {
    repos
        .iter()
        .map(|repo| {
            let head = head_of(repo)?;
            let blobs = match &head {
                Some(h) => read_tree(repo, h)?,
                None => BTreeMap::new(),
            };
            Ok(Tree {
                repo: repo.clone(),
                head,
                blobs,
            })
        })
        .collect()
}

fn matches(globs: &[Glob], path: &str) -> bool {
    globs.iter().any(|g| g.matches(path))
}

/// The `files` hash of `rows`, which come sorted by repo then path.
fn files_hash<'a>(rows: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(FILES_DOMAIN);
    for (repo, path, blob) in rows {
        for s in [repo, path, blob] {
            h.update(&(s.len() as u64).to_le_bytes());
            h.update(s.as_bytes());
        }
    }
    Hash(*h.finalize().as_bytes())
}

/// The snapshot of `globs` over `trees` (sorted by repo name). A file several globs match is
/// one row.
fn snapshot_of(trees: &[Tree], globs: &[Glob]) -> ReflectsSnapshot {
    let rows = trees.iter().flat_map(|t| {
        t.blobs
            .iter()
            .filter(|(path, _)| matches(globs, path))
            .map(|(path, blob)| (t.repo.configured.as_str(), path.as_str(), blob.as_str()))
    });
    ReflectsSnapshot {
        files: files_hash(rows),
        commits: trees
            .iter()
            .map(|t| PlanHead {
                repo: t.repo.configured.clone(),
                head: t.head.clone(),
            })
            .collect(),
    }
}

/// The code state `block`'s `reflects` globs match now in the linked checkouts of the project at
/// `root`. `None` when the block declares no reflects or no checkout is linked.
pub fn snapshot(root: &Path, block: &Block) -> Result<Option<ReflectsSnapshot>, RdyError> {
    let globs = glob_set(block);
    if globs.is_empty() {
        return Ok(None);
    }
    let repos = linked(root)?;
    if repos.is_empty() {
        return Ok(None);
    }
    Ok(Some(snapshot_of(&trees(&repos)?, &compiled(globs))))
}

/// The linked checkouts for one write, read once and only when a reflects block needs them.
struct Lazy<'r> {
    root: &'r Path,
    repos: Option<Vec<CodeRepo>>,
    trees: Option<Vec<Tree>>,
}

impl Lazy<'_> {
    fn repos(&mut self) -> Result<&[CodeRepo], RdyError> {
        if self.repos.is_none() {
            self.repos = Some(linked(self.root)?);
        }
        Ok(self.repos.as_deref().unwrap_or_default())
    }

    fn trees(&mut self) -> Result<&[Tree], RdyError> {
        if self.trees.is_none() {
            let t = trees(self.repos()?)?;
            self.trees = Some(t);
        }
        Ok(self.trees.as_deref().unwrap_or_default())
    }
}

/// D-01QX5 on a write: sets or drops `reflects_at` on the blocks of `ops`, against the block
/// each op starts from: an earlier op of the batch, else the version an update names as its
/// base (a conflict side's version is in `store`, while `base`, the view before the write, reads
/// the conflict's base), else nothing for a create. A block whose reflects becomes absent or
/// empty loses it; one that had none and still has none keeps what it holds. A created block,
/// one whose glob set changes, and one in `code` (a repin with `code: true`) get the current
/// snapshot, and so does one in `justified` (rf8, D-XC9B9: a block a `reflects` with
/// `code: true` justifies); any other rewrite keeps the one it has. With no linked
/// checkout nothing is made up: a block whose set changed loses its snapshot, a code repin is
/// `E_CONFIG`, a justified block keeps what it holds (its set changed or not), and each
/// reflects block written gets [`W_REFLECTS_NO_CODE`] once. Returns the warnings and the blocks
/// of `justified` stamped.
pub(crate) fn stamp(
    root: &Path,
    store: &rdy_store::Store,
    base: &Materialized,
    ops: &mut [Op],
    code: &BTreeSet<BlockId>,
    justified: &BTreeSet<BlockId>,
) -> Result<(Vec<Issue>, Vec<BlockId>), RdyError> {
    let mut lazy = Lazy {
        root,
        repos: None,
        trees: None,
    };
    // each block's glob set before the op at hand: the view's, then the batch's latest
    let mut prior: HashMap<BlockId, BTreeSet<String>> = HashMap::new();
    let mut warned: BTreeSet<BlockId> = BTreeSet::new();
    let mut out = vec![];
    let mut stamped = vec![];
    let owned =
        |set: BTreeSet<&str>| -> BTreeSet<String> { set.into_iter().map(str::to_string).collect() };
    for op in ops.iter_mut() {
        let (block, from) = match op {
            Op::Create { block } => (block, None),
            Op::Update { block, base: from } => (block, Some(*from)),
            Op::Delete { id, .. } => {
                prior.insert(id.clone(), BTreeSet::new());
                continue;
            }
        };
        let now = owned(glob_set(block));
        // the set the op starts from: a created block has none. A block with no reflects now
        // and no snapshot to drop needs no lookup.
        let was = match (prior.remove(&block.id), from) {
            (Some(was), _) => was,
            (None, None) => BTreeSet::new(),
            (None, Some(_)) if now.is_empty() && !block.fields.contains_key(REFLECTS_AT) => {
                BTreeSet::new()
            }
            (None, Some(v)) => match base.get(&block.id).filter(|b| b.version_hash() == v) {
                Some(b) => owned(glob_set(b)),
                None => owned(glob_set(&store.get_block(&v)?)),
            },
        };
        if now.is_empty() {
            // removing reflects removes the snapshot; a rewrite that leaves the set empty, as
            // it was, keeps whatever the block holds
            if !was.is_empty() {
                block.fields.remove(REFLECTS_AT);
            }
        } else {
            let changed = was != now;
            let repin = code.contains(&block.id);
            if lazy.repos()?.is_empty() {
                if repin {
                    return Err(RdyError::new(
                        E_CONFIG,
                        "no linked checkout for a code repin",
                        "set code_paths to a linked checkout, then repin with code: true",
                    ));
                }
                // a justified block keeps what it holds even when its glob set changes: with
                // no code state to record, dropping the old one would clear its code suspect
                if changed && !justified.contains(&block.id) {
                    block.fields.remove(REFLECTS_AT);
                }
                if warned.insert(block.id.clone()) {
                    out.push(Issue {
                        severity: Severity::Warning,
                        code: W_REFLECTS_NO_CODE,
                        block: Some(block.id.clone()),
                        message: format!(
                            "no linked checkout; rdy records no code state for {}",
                            block.id
                        ),
                        fix: NO_CODE_FIX.to_string(),
                    });
                }
            } else if changed || repin || justified.contains(&block.id) {
                let snap = snapshot_of(lazy.trees()?, &compiled(now.iter().map(String::as_str)));
                block
                    .fields
                    .insert(REFLECTS_AT.to_string(), snap.to_field());
                if justified.contains(&block.id) {
                    stamped.push(block.id.clone());
                }
            }
        }
        prior.insert(block.id.clone(), now);
    }
    Ok((out, stamped))
}

/// The blocks among `ids` (every block of the scope with `None`) whose reflected files differ
/// from their recorded snapshot, in id order. Only Canon, a view and the in-flight scope read the
/// checkout; a tag, landing or proposal scope gives none, and so do a block with no recorded
/// snapshot and a project with no linked checkout. A repo whose recorded commit this checkout
/// no longer holds still counts, with no paths of its own; nothing is fetched.
pub fn code_suspects(
    st: &ReadState,
    scope: &Scope,
    ids: Option<&[BlockId]>,
) -> Result<Vec<CodeSuspect>, RdyError> {
    match scope {
        Scope::Canon | Scope::View(_) | Scope::Inflight => {}
        Scope::At(_) | Scope::Proposal(_) => return Ok(vec![]),
    }
    let m = st.materialized(scope)?;
    let recorded = |b: &Block| -> Option<ReflectsAt> {
        if glob_set(b).is_empty() {
            return None;
        }
        decode_reflects_at(b.fields.get(REFLECTS_AT)?).ok()
    };
    let mut candidates: Vec<(&Block, ReflectsAt)> = match ids {
        Some(ids) => ids
            .iter()
            .filter_map(|id| m.get(id))
            .filter_map(|b| recorded(b).map(|r| (b, r)))
            .collect(),
        None => m
            .iter()
            .filter_map(|b| recorded(b).map(|r| (b, r)))
            .collect(),
    };
    if candidates.is_empty() {
        return Ok(vec![]);
    }
    candidates.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    candidates.dedup_by(|a, b| a.0.id == b.0.id);
    let repos = linked(st.root())?;
    if repos.is_empty() {
        return Ok(vec![]);
    }
    let now = trees(&repos)?;
    // each (repo, recorded commit)'s tree, read once; `None` when the checkout lacks the commit
    let mut then: HashMap<(String, String), Option<BTreeMap<String, String>>> = HashMap::new();
    let mut out = vec![];
    for (b, at) in candidates {
        let globs = compiled(glob_set(b));
        if snapshot_of(&now, &globs).files == at.files {
            continue;
        }
        let mut changed: BTreeSet<String> = BTreeSet::new();
        let names: BTreeSet<&str> = now
            .iter()
            .map(|t| t.repo.configured.as_str())
            .chain(at.commits.keys().map(String::as_str))
            .collect();
        for name in names {
            let tree = now.iter().find(|t| t.repo.configured == name);
            let empty = BTreeMap::new();
            let current = tree.map_or(&empty, |t| &t.blobs);
            // the files the repo matched at the recorded commit: none for a repo not linked
            // then or unborn then; unknown when the commit or the checkout is gone
            let old = match (at.commits.get(name), tree) {
                (None | Some(None), _) => Some(&empty),
                (Some(Some(_)), None) => None,
                (Some(Some(c)), Some(t)) => {
                    let key = (name.to_string(), c.clone());
                    if !then.contains_key(&key) {
                        let read = match commit_exists(&t.repo, c)? {
                            true => Some(read_tree(&t.repo, c)?),
                            false => None,
                        };
                        then.insert(key.clone(), read);
                    }
                    then[&key].as_ref()
                }
            };
            let Some(old) = old else { continue };
            let keys: BTreeSet<&String> = old
                .keys()
                .chain(current.keys())
                .filter(|p| matches(&globs, p))
                .collect();
            for path in keys {
                if old.get(path) != current.get(path) {
                    changed.insert(format!("{name}:{path}"));
                }
            }
        }
        out.push(CodeSuspect {
            block: b.id.clone(),
            paths: changed.into_iter().collect(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rdy;
    use crate::consequence::{consequence_id, write_and_report};
    use crate::engine::Engine;
    use crate::write::parse_ops;
    use crate::{E_BAD_INPUT, E_CONFIG, Local, Session, SessionKind, WriteRequest, WriteResult};
    use rdy_model::engine_fields::{REFLECTS_AT, decode_reflects_at};
    use rdy_model::{FieldValue, Role};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::process::Command;

    const VIEW: &str = "work";

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=qa",
                "-c",
                "user.email=qa",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Writes each `(path, text)` (an empty text deletes the path), commits them all, and
    /// returns the new HEAD.
    fn commit(dir: &Path, files: &[(&str, &str)], message: &str) -> String {
        for (path, text) in files {
            let p = dir.join(path);
            if text.is_empty() {
                let _ = std::fs::remove_file(&p);
            } else {
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, text).unwrap();
            }
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "--no-verify", "-m", message]);
        git(dir, &["rev-parse", "HEAD"])
    }

    fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        commit(d.path(), files, "first");
        d
    }

    fn blob(dir: &Path, path: &str) -> String {
        git(dir, &["rev-parse", &format!("HEAD:{path}")])
    }

    /// The expected `files` hash of sorted `(repo, path, blob)` rows, framed independently.
    fn files_of(rows: &[(&str, &str, &str)]) -> Hash {
        let mut bytes = b"rdy-reflects-files\0".to_vec();
        for (r, p, b) in rows {
            for s in [r, p, b] {
                bytes.extend_from_slice(&(s.len() as u64).to_le_bytes());
                bytes.extend_from_slice(s.as_bytes());
            }
        }
        Hash::of(&bytes)
    }

    fn session() -> Session {
        Session {
            actor: "agent:designer".into(),
            run: Some("run-1".into()),
            role: Role::Writer,
            kind: SessionKind::Human,
        }
    }

    /// Links `repos` (name, checkout) into the project at `dir`, replacing any earlier links.
    fn link(dir: &Path, repos: &[(&str, &Path)]) {
        let yaml = dir.join(".rdy.yaml");
        let text = std::fs::read_to_string(&yaml).unwrap();
        let kept: Vec<&str> = text
            .lines()
            .filter(|l| !l.starts_with("code_repos:"))
            .collect();
        let names: Vec<&str> = repos.iter().map(|(n, _)| *n).collect();
        let mut text = kept.join("\n");
        text.push_str(&format!("\ncode_repos: [{}]\n", names.join(", ")));
        std::fs::write(&yaml, text).unwrap();
        std::fs::create_dir_all(dir.join(".rdy")).unwrap();
        let mut local = "code_paths:\n".to_string();
        for (n, p) in repos {
            local.push_str(&format!("  {n}: {}\n", p.display()));
        }
        std::fs::write(dir.join(".rdy/local.yaml"), local).unwrap();
    }

    /// Drops every `code_paths` entry, so no checkout is linked (the names stay listed).
    fn unlink(dir: &Path) {
        std::fs::write(dir.join(".rdy/local.yaml"), "code_paths: {}\n").unwrap();
    }

    fn project(repos: &[(&str, &Path)]) -> (tempfile::TempDir, Rdy) {
        let dir = tempfile::tempdir().unwrap();
        let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        if !repos.is_empty() {
            link(dir.path(), repos);
        }
        (dir, r)
    }

    fn request(ops: Value) -> WriteRequest {
        let Value::Array(ops) = ops else {
            panic!("ops are a list")
        };
        WriteRequest {
            view: VIEW.into(),
            ops: parse_ops(ops).unwrap(),
            justification: None,
            message: "cp9".into(),
        }
    }

    fn write(r: &mut Rdy, ops: Value) -> Result<WriteResult, RdyError> {
        write_in(r, VIEW, ops)
    }

    fn write_in(r: &mut Rdy, view: &str, ops: Value) -> Result<WriteResult, RdyError> {
        let ops = match ops {
            Value::Array(ops) => ops,
            _ => panic!("ops are a list"),
        };
        let ops = parse_ops(ops)?;
        r.write(
            &session(),
            WriteRequest {
                view: view.into(),
                ops,
                justification: None,
                message: "cp9".into(),
            },
        )
    }

    fn id(w: &WriteResult, placeholder: &str) -> BlockId {
        BlockId::parse(&w.created[placeholder]).unwrap()
    }

    /// A doc and section, then claims under the section: `(placeholder, body, reflects)`.
    fn claims(r: &mut Rdy, items: &[(&str, &str, Option<Value>)]) -> WriteResult {
        let mut ops = vec![
            json!({"op": "create", "type": "doc", "id": "$d",
                "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}}),
            json!({"op": "create", "type": "section", "id": "$s", "parent": "$d",
                "fields": {"title": "Rules"}}),
        ];
        for (p, body, reflects) in items {
            let mut op = json!({"op": "create", "type": "claim", "id": p, "parent": "$s",
                "body": body});
            if let Some(g) = reflects {
                op["fields"] = json!({"reflects": g});
            }
            ops.push(op);
        }
        write(r, Value::Array(ops)).unwrap()
    }

    fn view() -> Scope {
        Scope::View(VIEW.into())
    }

    fn block_of(st: &ReadState, b: &BlockId) -> Block {
        st.materialized(&view()).unwrap().get(b).unwrap().clone()
    }

    fn stored(st: &ReadState, b: &BlockId) -> Option<rdy_model::engine_fields::ReflectsAt> {
        block_of(st, b)
            .fields
            .get(REFLECTS_AT)
            .map(|v| decode_reflects_at(v).unwrap())
    }

    fn heads(rows: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        rows.iter()
            .map(|(r, h)| (r.to_string(), h.map(str::to_string)))
            .collect()
    }

    fn suspects(st: &ReadState) -> Vec<CodeSuspect> {
        code_suspects(st, &view(), None).unwrap()
    }

    fn paths(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn cp9_create_and_changed_globs_record_blob_state() {
        let app = repo(&[
            ("src/a.rs", "a\n"),
            ("src/b.rs", "b\n"),
            ("docs/x.md", "x\n"),
            ("lib/z.rs", "z\n"),
        ]);
        let web = repo(&[("src/w.rs", "w\n")]);
        let fresh = tempfile::tempdir().unwrap();
        git(fresh.path(), &["init", "-q"]);
        // listed out of name order: rows sort by repo, then path
        let (dir, mut r) = project(&[
            ("web", web.path()),
            ("app", app.path()),
            ("unborn", fresh.path()),
        ]);
        // a.rs is matched by both globs and hashed once
        let w = claims(
            &mut r,
            &[("$r", "R.", Some(json!(["src/*.rs", "src/a.rs"])))],
        );
        let rb = id(&w, "$r");
        let (a, b, ww) = (
            blob(app.path(), "src/a.rs"),
            blob(app.path(), "src/b.rs"),
            blob(web.path(), "src/w.rs"),
        );
        let app_head = git(app.path(), &["rev-parse", "HEAD"]);
        let web_head = git(web.path(), &["rev-parse", "HEAD"]);
        let at = stored(&r.read(), &rb).expect("a create records the snapshot");
        assert_eq!(
            at.files,
            files_of(&[
                ("app", "src/a.rs", &a),
                ("app", "src/b.rs", &b),
                ("web", "src/w.rs", &ww)
            ])
        );
        assert_eq!(
            at.commits,
            heads(&[
                ("app", Some(&app_head)),
                ("unborn", None),
                ("web", Some(&web_head))
            ])
        );
        // the author's field is not rewritten
        assert_eq!(
            block_of(&r.read(), &rb).fields.get("reflects"),
            Some(&FieldValue::List(vec![
                FieldValue::Str("src/*.rs".into()),
                FieldValue::Str("src/a.rs".into())
            ]))
        );
        // the public helper reads the same state
        let snap = snapshot(dir.path(), &block_of(&r.read(), &rb))
            .unwrap()
            .unwrap();
        assert_eq!(snap.files, at.files);
        assert_eq!(
            snap.commits,
            vec![
                PlanHead {
                    repo: "app".into(),
                    head: Some(app_head.clone())
                },
                PlanHead {
                    repo: "unborn".into(),
                    head: None
                },
                PlanHead {
                    repo: "web".into(),
                    head: Some(web_head.clone())
                },
            ]
        );

        // a changed glob set records the new state
        let head2 = commit(app.path(), &[("docs/x.md", "x2\n")], "docs");
        write(
            &mut r,
            json!([{"op": "update", "id": rb.to_string(), "fields": {"reflects": ["docs/*.md"]}}]),
        )
        .unwrap();
        let at = stored(&r.read(), &rb).unwrap();
        assert_eq!(
            at.files,
            files_of(&[("app", "docs/x.md", &blob(app.path(), "docs/x.md"))])
        );
        assert_eq!(at.commits["app"].as_deref(), Some(head2.as_str()));

        // a glob set that matches nothing has the domain's hash
        write(
            &mut r,
            json!([{"op": "update", "id": rb.to_string(), "fields": {"reflects": ["none/*.rs"]}}]),
        )
        .unwrap();
        assert_eq!(
            stored(&r.read(), &rb).unwrap().files,
            Hash::of(b"rdy-reflects-files\0")
        );

        // removing reflects removes reflects_at
        write(
            &mut r,
            json!([{"op": "update", "id": rb.to_string(), "unset": ["reflects"]}]),
        )
        .unwrap();
        assert_eq!(stored(&r.read(), &rb), None);
        assert!(suspects(&r.read()).is_empty());
    }

    #[test]
    fn cp9_dirty_worktree_and_unrelated_commit_do_not_raise_suspect() {
        let app = repo(&[("src/a.rs", "a\n"), ("docs/x.md", "x\n")]);
        let (_dir, mut r) = project(&[("app", app.path())]);
        let w = claims(&mut r, &[("$r", "R.", Some(json!(["src/*.rs"])))]);
        let rb = id(&w, "$r");
        let before = stored(&r.read(), &rb).unwrap();
        // an unrelated commit moves HEAD, but the matched blobs stay the same
        commit(app.path(), &[("docs/x.md", "x2\n")], "docs");
        // worktree and staged bytes play no part
        std::fs::write(app.path().join("src/a.rs"), "dirty\n").unwrap();
        std::fs::write(app.path().join("src/new.rs"), "staged\n").unwrap();
        git(app.path(), &["add", "src/new.rs"]);
        assert!(suspects(&r.read()).is_empty());
        assert_eq!(
            snapshot(_dir.path(), &block_of(&r.read(), &rb))
                .unwrap()
                .unwrap()
                .files,
            before.files
        );
    }

    #[test]
    fn cp9_matched_add_change_delete_and_rename_raise_paths() {
        let app = repo(&[
            ("src/a.rs", "a\n"),
            ("src/b.rs", "b\n"),
            ("src/d.rs", "d is renamed and stays the same\n"),
            ("src/f.rs", "f\n"),
            ("docs/x.md", "x\n"),
        ]);
        let (_dir, mut r) = project(&[("app", app.path())]);
        let w = claims(
            &mut r,
            &[
                ("$r", "R.", Some(json!(["src/*.rs"]))),
                ("$q", "Q.", Some(json!(["docs/*.md"]))),
            ],
        );
        let (rb, qb) = (id(&w, "$r"), id(&w, "$q"));
        git(app.path(), &["mv", "src/d.rs", "src/e.rs"]);
        commit(
            app.path(),
            &[
                ("src/a.rs", "a2\n"),
                ("src/b.rs", ""),
                ("src/c.rs", "c\n"),
                ("docs/x.md", "x\n"),
            ],
            "change, delete, add, rename",
        );
        let st = r.read();
        assert_eq!(
            suspects(&st),
            vec![CodeSuspect {
                block: rb.clone(),
                paths: paths(&[
                    "app:src/a.rs",
                    "app:src/b.rs",
                    "app:src/c.rs",
                    "app:src/d.rs",
                    "app:src/e.rs"
                ]),
            }]
        );
        // the ids restrict the read
        assert!(
            code_suspects(&st, &view(), Some(std::slice::from_ref(&qb)))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            code_suspects(&st, &view(), Some(std::slice::from_ref(&rb)))
                .unwrap()
                .len(),
            1
        );
        // historical scopes never consult the checkout
        for scope in [Scope::At(Hash([7; 32])), Scope::Proposal(Hash([7; 32]))] {
            assert!(code_suspects(&st, &scope, None).unwrap().is_empty());
        }
    }

    #[test]
    fn cp9_unchanged_glob_rewrite_keeps_snapshot_and_reports_suspect() {
        let app = repo(&[("src/a.rs", "a\n")]);
        let (_dir, mut r) = project(&[("app", app.path())]);
        let w = claims(
            &mut r,
            &[
                ("$t", "T holds.", None),
                ("$r", "R follows {{$t}}.", Some(json!(["src/*.rs"]))),
            ],
        );
        let (tb, rb) = (id(&w, "$t"), id(&w, "$r"));
        let before = stored(&r.read(), &rb).unwrap();
        commit(app.path(), &[("src/a.rs", "a2\n")], "drift");
        // T changes, so R is also a structural suspect on T
        write(
            &mut r,
            json!([{"op": "update", "id": tb.to_string(), "body": "T holds, changed."}]),
        )
        .unwrap();
        let e = Local::new(r);
        let body = format!("R still follows {{{{{tb}}}}}.");
        let req = request(json!([{"op": "update", "id": rb.to_string(), "body": body}]));
        let s = session();
        let v = write_and_report(&e, VIEW, None, move |r| r.write(&s, req)).unwrap();
        let st = e.read();
        assert_eq!(
            stored(&st, &rb).unwrap(),
            before,
            "a body rewrite keeps the snapshot"
        );
        let change: Hash = v["change"].as_str().unwrap().parse().unwrap();
        let rows = v["consequences"].as_array().unwrap();
        let mine: Vec<&Value> = rows
            .iter()
            .filter(|c| c["block"] == rb.to_string())
            .collect();
        assert_eq!(mine.len(), 2, "{rows:#?}");
        // the structural kept row first, with its target
        assert_eq!(mine[0]["kind"], "suspect");
        assert_eq!(mine[0]["because"]["changed"], tb.to_string());
        // then the code row, last of all
        let want = json!({
            "id": consequence_id(&change, "suspect code_changed", &rb),
            "block": rb.to_string(),
            "kind": "suspect",
            "because": {"edge": "reflects", "changed": null},
            "follow_up": "confirm",
            "reason": "code_changed",
            "target": null,
            "paths": ["app:src/a.rs"],
            "already_written": true,
            "fix": "check the block against its code, then repin it with code: true",
        });
        assert_eq!(rows.last().unwrap(), &want);
        assert_eq!(
            serde_json::to_string(rows.last().unwrap()).unwrap(),
            serde_json::to_string(&want).unwrap(),
            "the fields come in their pinned order"
        );
        // the report cutter budgets the code row with the others
        let req = request(json!([{"op": "update", "id": rb.to_string(), "body": "R again."}]));
        let s = session();
        let v = write_and_report(&e, VIEW, Some(1), move |r| r.write(&s, req)).unwrap();
        assert_eq!(v["consequences"], json!([]));
        assert!(v["consequences_omitted"]["suspect"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn cp9_code_repin_clears_only_code_drift() {
        let app = repo(&[("src/a.rs", "a\n")]);
        let (dir, mut r) = project(&[("app", app.path())]);
        let w = claims(
            &mut r,
            &[
                ("$t1", "T1 holds.", None),
                ("$t2", "T2 holds.", None),
                (
                    "$r",
                    "R follows {{$t1}} and {{$t2}}.",
                    Some(json!(["src/*.rs"])),
                ),
                ("$q", "Q cites nothing.", Some(json!(["src/*.rs"]))),
            ],
        );
        let (t1, t2, rb, qb) = (id(&w, "$t1"), id(&w, "$t2"), id(&w, "$r"), id(&w, "$q"));
        let head2 = commit(app.path(), &[("src/a.rs", "a2\n")], "drift");
        write(
            &mut r,
            json!([{"op": "update", "id": t1.to_string(), "body": "T1 holds, changed."}]),
        )
        .unwrap();
        let st = r.read();
        assert_eq!(suspects(&st).len(), 2);
        let design: Vec<_> = st
            .suspects(&view())
            .unwrap()
            .into_iter()
            .map(|s| (s.block, s.target))
            .collect();
        assert_eq!(design, vec![(rb.clone(), t1.clone())]);
        let tracking = r.store.tracking().clone();
        let (content, version) = {
            let b = block_of(&st, &rb);
            (b.content_hash_with(&tracking), b.version_hash())
        };

        // the selection: read-only, counting already-current pins
        let end = r.store.log_end();
        let s = session();
        assert_eq!(
            r.repin_selection(&s, VIEW, rb.as_str(), Some(&[t2.to_string()]))
                .unwrap(),
            (rb.clone(), 1)
        );
        assert_eq!(
            r.repin_selection(&s, VIEW, rb.as_str(), None).unwrap(),
            (rb.clone(), 2)
        );
        let e = r
            .repin_selection(&s, VIEW, rb.as_str(), Some(&[qb.to_string()]))
            .unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT);
        assert_eq!(
            e.message,
            format!("{rb} does not reference or link to {qb}")
        );
        assert_eq!(r.store.log_end(), end);

        // a code repin of an already-current target: only the code drift clears
        write(
            &mut r,
            json!([{"op": "repin", "id": rb.to_string(), "targets": [t2.to_string()], "code": true}]),
        )
        .unwrap();
        let st = r.read();
        let b = block_of(&st, &rb);
        assert_eq!(b.content_hash_with(&tracking), content);
        assert_ne!(b.version_hash(), version);
        let at = stored(&st, &rb).unwrap();
        assert_eq!(at.commits["app"].as_deref(), Some(head2.as_str()));
        assert_eq!(
            at.files,
            files_of(&[("app", "src/a.rs", &blob(app.path(), "src/a.rs"))])
        );
        assert_eq!(
            suspects(&st),
            vec![CodeSuspect {
                block: qb.clone(),
                paths: paths(&["app:src/a.rs"])
            }]
        );
        let design: Vec<_> = st
            .suspects(&view())
            .unwrap()
            .into_iter()
            .map(|s| (s.block, s.target))
            .collect();
        assert_eq!(design, vec![(rb.clone(), t1.clone())]);

        // a code-only repin with no design pin is a valid change
        write(
            &mut r,
            json!([{"op": "repin", "id": qb.to_string(), "code": true}]),
        )
        .unwrap();
        assert!(suspects(&r.read()).is_empty());

        // a plain repin that changes no pin is still refused
        let e = write(&mut r, json!([{"op": "repin", "id": qb.to_string()}])).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT);

        // a block with no reflects has no code to repin
        let e = write(
            &mut r,
            json!([{"op": "repin", "id": t1.to_string(), "code": true}]),
        )
        .unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_BAD_INPUT,
                format!("{t1} has no reflects globs").as_str(),
                "set reflects before repinning code"
            )
        );
        // same-batch content-target checks still apply to design pins
        let e = write(
            &mut r,
            json!([
                {"op": "update", "id": t2.to_string(), "body": "T2 changes here."},
                {"op": "repin", "id": rb.to_string(), "targets": [t2.to_string()], "code": true}
            ]),
        )
        .unwrap_err();
        assert_eq!(
            e.message,
            format!("{t2} changes in this write, so a repin to it would pin an old version")
        );
        drop(dir);
    }

    #[test]
    fn cp9_no_checkout_warns_on_write_and_refuses_repin() {
        let (dir, mut r) = project(&[]);
        let w = claims(&mut r, &[("$r", "R.", Some(json!(["src/*.rs"])))]);
        let rb = id(&w, "$r");
        let warned: Vec<&Issue> = w
            .warnings
            .iter()
            .filter(|i| i.code == W_REFLECTS_NO_CODE)
            .collect();
        assert_eq!(warned.len(), 1, "{:?}", w.warnings);
        assert_eq!(warned[0].block.as_ref(), Some(&rb));
        assert_eq!(warned[0].severity, rdy_schema::Severity::Warning);
        assert_eq!(
            warned[0].message,
            format!("no linked checkout; rdy records no code state for {rb}")
        );
        assert_eq!(
            warned[0].fix,
            "set code_paths, then repin it with code: true"
        );
        assert_eq!(stored(&r.read(), &rb), None, "no snapshot is made up");
        assert!(suspects(&r.read()).is_empty());
        let e = write(
            &mut r,
            json!([{"op": "repin", "id": rb.to_string(), "code": true}]),
        )
        .unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_CONFIG,
                "no linked checkout for a code repin",
                "set code_paths to a linked checkout, then repin with code: true"
            )
        );

        // a snapshot made while linked: kept on a body rewrite, dropped on a glob change
        let app = repo(&[("src/a.rs", "a\n"), ("src/b.rs", "b\n")]);
        link(dir.path(), &[("app", app.path())]);
        write(
            &mut r,
            json!([{"op": "repin", "id": rb.to_string(), "code": true}]),
        )
        .unwrap();
        let at = stored(&r.read(), &rb).expect("a code repin records the snapshot");
        unlink(dir.path());
        let w = write(
            &mut r,
            json!([{"op": "update", "id": rb.to_string(), "body": "R, again."}]),
        )
        .unwrap();
        assert!(w.warnings.iter().any(|i| i.code == W_REFLECTS_NO_CODE));
        assert_eq!(stored(&r.read(), &rb), Some(at));
        assert!(
            suspects(&r.read()).is_empty(),
            "no checkout: no code suspect"
        );
        let w = write(
            &mut r,
            json!([{"op": "update", "id": rb.to_string(), "fields": {"reflects": ["src/a.rs"]}}]),
        )
        .unwrap();
        assert!(w.warnings.iter().any(|i| i.code == W_REFLECTS_NO_CODE));
        assert_eq!(stored(&r.read(), &rb), None);

        // a configured checkout that is no repository is not hidden behind the warning
        let plain = tempfile::tempdir().unwrap();
        link(dir.path(), &[("app", plain.path())]);
        let e = write(
            &mut r,
            json!([{"op": "update", "id": rb.to_string(), "body": "R, once more."}]),
        )
        .unwrap_err();
        assert_eq!(e.code, E_CONFIG);
        assert_eq!(e.message, "code repo app is not a git repository");
        // a write of a block with no reflects reads no code config
        let w = write(
            &mut r,
            json!([{"op": "create", "type": "doc", "id": "$o",
                "fields": {"name": "other", "title": "Other", "doc_type": "overview"}}]),
        )
        .unwrap();
        assert!(w.warnings.iter().all(|i| i.code != W_REFLECTS_NO_CODE));
    }

    #[test]
    fn cp9_missing_recorded_commit_keeps_suspect_without_fetch() {
        let upstream = repo(&[("src/a.rs", "a\n")]);
        let local = tempfile::tempdir().unwrap();
        let lp = local.path().join("app");
        git(
            local.path(),
            &[
                "clone",
                "-q",
                "--no-local",
                upstream.path().to_str().unwrap(),
                lp.to_str().unwrap(),
            ],
        );
        let (_dir, mut r) = project(&[("app", lp.as_path())]);
        let w = claims(&mut r, &[("$r", "R.", Some(json!(["src/*.rs"])))]);
        let rb = id(&w, "$r");
        let first = stored(&r.read(), &rb).unwrap().commits["app"]
            .clone()
            .unwrap();
        // rewrite the local history so the recorded commit is gone; upstream still holds it
        git(&lp, &["checkout", "-q", "--orphan", "fresh"]);
        commit(&lp, &[("src/a.rs", "other\n")], "fresh history");
        git(&lp, &["branch", "-q", "-D", "main"]);
        git(&lp, &["remote", "remove", "origin"]);
        git(
            &lp,
            &["remote", "add", "origin", upstream.path().to_str().unwrap()],
        );
        git(&lp, &["reflog", "expire", "--expire=now", "--all"]);
        git(&lp, &["gc", "-q", "--prune=now"]);
        let gone = |what: &str| {
            !Command::new("git")
                .arg("-C")
                .arg(&lp)
                .args(["cat-file", "-e", &format!("{what}^{{commit}}")])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        assert!(gone(&first), "the recorded commit is gone locally");
        assert_eq!(
            suspects(&r.read()),
            vec![CodeSuspect {
                block: rb,
                paths: vec![]
            }]
        );
        assert!(gone(&first), "nothing was fetched");
    }

    #[test]
    fn cp9_code_drift_writes_no_plan_or_transition() {
        let app = repo(&[("src/a.rs", "a\n")]);
        let (_dir, mut r) = project(&[("app", app.path())]);
        let w = claims(&mut r, &[("$r", "R.", Some(json!(["src/*.rs"])))]);
        let rb = id(&w, "$r");
        commit(app.path(), &[("src/a.rs", "a2\n")], "drift");
        let open = r.store.open_suspects().unwrap();
        let end = r.store.log_end();
        let st = r.read();
        assert_eq!(suspects(&st).len(), 1);
        assert_eq!(suspects(&st).len(), 1);
        assert_eq!(r.store.log_end(), end, "a read writes nothing");
        assert!(st.suspects(&view()).unwrap().is_empty());
        // an unchanged-glob rewrite logs the change only
        write(
            &mut r,
            json!([{"op": "update", "id": rb.to_string(), "body": "R, rewritten."}]),
        )
        .unwrap();
        let kinds: Vec<u8> = r.store.last_batch().iter().map(|(k, _)| *k).collect();
        assert!(!kinds.contains(&rdy_store::kinds::PLAN), "{kinds:?}");
        assert!(!kinds.contains(&rdy_store::kinds::TRANSITION), "{kinds:?}");
        assert_eq!(r.store.open_suspects().unwrap(), open);
        let st = r.read();
        assert!(st.suspects(&view()).unwrap().is_empty());
        assert_eq!(
            suspects(&st).len(),
            1,
            "the code suspect is a read, still there"
        );
    }

    #[test]
    fn cp9_equivalent_glob_order_does_not_repin() {
        let app = repo(&[("src/a.rs", "a\n"), ("src/b.rs", "b\n")]);
        let (_dir, mut r) = project(&[("app", app.path())]);
        let w = claims(
            &mut r,
            &[("$r", "R.", Some(json!(["src/b.rs", "src/a.rs"])))],
        );
        let rb = id(&w, "$r");
        let first = stored(&r.read(), &rb).unwrap();
        commit(app.path(), &[("src/a.rs", "a2\n")], "drift");
        let reordered = json!(["src/a.rs", "src/b.rs", "src/a.rs"]);
        write(
            &mut r,
            json!([{"op": "update", "id": rb.to_string(), "fields": {"reflects": reordered}}]),
        )
        .unwrap();
        let st = r.read();
        assert_eq!(
            stored(&st, &rb),
            Some(first),
            "the same set keeps the snapshot"
        );
        assert_eq!(
            block_of(&st, &rb)
                .fields
                .get("reflects")
                .and_then(|v| match v {
                    FieldValue::List(l) => Some(l.len()),
                    _ => None,
                }),
            Some(3),
            "the author's field is kept as written"
        );
        assert_eq!(
            suspects(&st),
            vec![CodeSuspect {
                block: rb.clone(),
                paths: paths(&["app:src/a.rs"])
            }]
        );
        // a real change of the set records the state again
        write(
            &mut r,
            json!([{"op": "update", "id": rb.to_string(), "fields": {"reflects": ["src/a.rs"]}}]),
        )
        .unwrap();
        assert!(suspects(&r.read()).is_empty());
    }

    /// R reflects src/a.rs in view `work`; `w2` forks it. Side A (`work`) changes R's globs to
    /// src/b.rs, side B (`w2`) changes its body, and `work` includes B's change, so R is
    /// conflicted in `work` with the original version as its base. Then src/b.rs changes in a
    /// commit. Returns the repo, the project, R, and side A's version.
    fn conflicted_on_globs() -> (tempfile::TempDir, tempfile::TempDir, Rdy, BlockId, Hash) {
        let app = repo(&[("src/a.rs", "a\n"), ("src/b.rs", "b\n")]);
        let (dir, mut r) = project(&[("app", app.path())]);
        let w = claims(&mut r, &[("$r", "R.", Some(json!(["src/a.rs"])))]);
        let rb = id(&w, "$r");
        let s = session();
        r.view_fork(&s, VIEW, "w2").unwrap();
        write(
            &mut r,
            json!([{"op": "update", "id": rb.to_string(), "fields": {"reflects": ["src/b.rs"]}}]),
        )
        .unwrap();
        let va = block_of(&r.read(), &rb).version_hash();
        assert_eq!(
            stored(&r.read(), &rb).unwrap().files,
            files_of(&[("app", "src/b.rs", &blob(app.path(), "src/b.rs"))]),
            "side A records src/b.rs"
        );
        let cb = write_in(
            &mut r,
            "w2",
            json!([{"op": "update", "id": rb.to_string(), "body": "R, side B."}]),
        )
        .unwrap()
        .change;
        r.view_include(&s, VIEW, &[cb]).unwrap();
        assert!(
            r.read().conflict_info(&view(), &rb).unwrap().is_some(),
            "R is conflicted in work"
        );
        commit(
            app.path(),
            &[("src/b.rs", "b2\n")],
            "drift on side A's file",
        );
        (app, dir, r, rb, va)
    }

    #[test]
    fn cp9_conflict_side_body_update_keeps_the_side_snapshot() {
        let (app, _dir, mut r, rb, va) = conflicted_on_globs();
        let first = git(app.path(), &["rev-parse", "HEAD~1"]);
        let old_b = git(app.path(), &["rev-parse", "HEAD~1:src/b.rs"]);
        // resolve from side A with a body-only update: its globs (src/b.rs) do not change
        write(
            &mut r,
            json!([{"op": "update", "id": rb.to_string(), "base": va.to_string(),
                "body": "R, resolved from side A."}]),
        )
        .unwrap();
        let st = r.read();
        assert!(st.conflict_info(&view(), &rb).unwrap().is_none());
        let at = stored(&st, &rb).unwrap();
        assert_eq!(
            at.files,
            files_of(&[("app", "src/b.rs", &old_b)]),
            "side A's snapshot is kept"
        );
        assert_eq!(at.commits["app"].as_deref(), Some(first.as_str()));
        assert_eq!(
            suspects(&st),
            vec![CodeSuspect {
                block: rb,
                paths: paths(&["app:src/b.rs"])
            }],
            "the unchecked code drift stays suspect"
        );
    }

    #[test]
    fn cp9_conflict_side_glob_change_records_a_new_snapshot() {
        let (app, _dir, mut r, rb, va) = conflicted_on_globs();
        let head = git(app.path(), &["rev-parse", "HEAD"]);
        // from side A, back to the conflict base's globs: a change of side A's set
        write(
            &mut r,
            json!([{"op": "update", "id": rb.to_string(), "base": va.to_string(),
                "fields": {"reflects": ["src/a.rs"]}}]),
        )
        .unwrap();
        let st = r.read();
        let at = stored(&st, &rb).unwrap();
        assert_eq!(
            at.files,
            files_of(&[("app", "src/a.rs", &blob(app.path(), "src/a.rs"))]),
            "the snapshot follows the new globs"
        );
        assert_eq!(at.commits["app"].as_deref(), Some(head.as_str()));
        assert!(suspects(&st).is_empty(), "{:?}", suspects(&st));
    }

    #[test]
    fn cp9_code_read_failure_keeps_structural_rows() {
        let app = repo(&[("src/a.rs", "a\n")]);
        let (_dir, mut r) = project(&[("app", app.path())]);
        let w = claims(
            &mut r,
            &[
                ("$t", "T holds.", None),
                ("$r", "R follows {{$t}}.", Some(json!(["src/*.rs"]))),
            ],
        );
        let (tb, rb) = (id(&w, "$t"), id(&w, "$r"));
        write(
            &mut r,
            json!([{"op": "update", "id": tb.to_string(), "body": "T holds, changed."}]),
        )
        .unwrap();
        // the branch HEAD names now points at a blob: the code read fails
        let b = blob(app.path(), "src/a.rs");
        let branch = git(app.path(), &["symbolic-ref", "HEAD"]);
        std::fs::write(app.path().join(".git").join(&branch), format!("{b}\n")).unwrap();
        let e = Local::new(r);
        let body = format!("R still follows {{{{{tb}}}}}.");
        let req = request(json!([{"op": "update", "id": rb.to_string(), "body": body}]));
        let s = session();
        let v = write_and_report(&e, VIEW, None, move |r| r.write(&s, req)).unwrap();
        assert!(v.get("consequences_error").is_none(), "{v:#}");
        let rows = v["consequences"].as_array().unwrap();
        let mine: Vec<&Value> = rows
            .iter()
            .filter(|c| c["block"] == rb.to_string())
            .collect();
        assert_eq!(mine.len(), 1, "{v:#}");
        assert_eq!(mine[0]["kind"], "suspect");
        assert_eq!(mine[0]["because"]["changed"], tb.to_string());
        assert!(
            v["consequences_code_error"]
                .as_str()
                .is_some_and(|t| t.starts_with("E_IO: ")),
            "{v:#}"
        );
    }

    #[test]
    fn cp9_repin_selection_counts_distinct_targets() {
        let app = repo(&[("src/a.rs", "a\n")]);
        let (_dir, mut r) = project(&[("app", app.path())]);
        let w = write(
            &mut r,
            json!([
                {"op": "create", "type": "doc", "id": "$d",
                    "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d",
                    "fields": {"title": "Rules"}},
                {"op": "create", "type": "claim", "id": "$t", "parent": "$s",
                    "fields": {"key": "tee"}, "body": "T holds."},
                {"op": "create", "type": "claim", "id": "$r", "parent": "$s",
                    "body": "R follows {{$t}}."}
            ]),
        )
        .unwrap();
        let (tb, rb) = (id(&w, "$t"), id(&w, "$r"));
        let s = session();
        let count = |targets: &[String]| r.repin_selection(&s, VIEW, rb.as_str(), Some(targets));
        assert_eq!(
            count(&[tb.to_string(), tb.to_string()]).unwrap(),
            (rb.clone(), 1),
            "a repeated target is one pin"
        );
        assert_eq!(
            count(&[tb.to_string(), "tee".to_string()]).unwrap(),
            (rb.clone(), 1),
            "an id and a key of the same target are one pin"
        );
        // the target checks still run on every argument
        let e = count(&[tb.to_string(), rb.to_string()]).unwrap_err();
        assert_eq!(e.message, format!("{rb} is never pinned to itself"));
    }
}
