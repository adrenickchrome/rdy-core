use crate::Materialized;
use crate::error::*;
use crate::keys::Keys;
use crate::query::field_json;
use crate::tree_io::{self, FOLDER_FILE};
use crate::write::{LinkInput, OpInput, TRACKING_NEEDS_REASON, WriteRequest, WriteResult};
use crate::{Rdy, ReadState, Scope, Session};
use rdy_model::{
    ActorRun, Block, BlockId, Change, ChangeLinks, EditorialClass, FieldValue, Hash, Justification,
    Link, Op, Provenance, Role, tree,
};
use rdy_schema::{Pack, has_errors, skeleton_gaps, validate_block, validate_prefix};
use serde::{Deserialize, Serialize};
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Serialize, Deserialize)]
pub(crate) struct LinkYaml {
    target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    part: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BlockYaml {
    pub(crate) id: BlockId,
    #[serde(rename = "type")]
    pub(crate) block_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent: Option<BlockId>,
    pub(crate) order: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) fields: BTreeMap<String, serde_yaml_ng::Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) body: String,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "link_yaml_map"
    )]
    pub(crate) links: BTreeMap<String, Vec<LinkYaml>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) pins: BTreeMap<BlockId, String>,
}

/// llst1: a link kind is a string, a list of strings, or a list of `{target, part?}` objects.
fn link_yaml_map<'de, D>(d: D) -> Result<BTreeMap<String, Vec<LinkYaml>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(crate::write::link_map(d)?
        .into_iter()
        .map(|(k, v)| {
            let v = v
                .into_iter()
                .map(|l| LinkYaml {
                    target: l.target,
                    part: l.part,
                })
                .collect();
            (k, v)
        })
        .collect())
}

/// lkey1: a link target written as a key, held until the keys of the import or the view can
/// resolve it. `at` is its position in the kind's list.
pub struct KeyedLink {
    kind: String,
    at: usize,
    text: String,
    part: Option<String>,
}

impl KeyedLink {
    /// The text a keyed target was written as.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The link kind it was written under.
    #[allow(dead_code)] // read by the working-copy layer
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Its position in the kind's list.
    #[allow(dead_code)] // read by the working-copy layer
    pub fn at(&self) -> usize {
        self.at
    }

    /// The part it names, if any.
    #[allow(dead_code)] // read by the working-copy layer
    pub fn part(&self) -> Option<&str> {
        self.part.as_deref()
    }
}

/// lkey1: puts each keyed target in its place in `b`'s link lists, as the id `lookup` gives.
pub(crate) fn place_keyed(
    b: &mut Block,
    mut keyed: Vec<KeyedLink>,
    lookup: &dyn Fn(&str) -> Result<Option<BlockId>, RdyError>,
) -> Result<(), RdyError> {
    keyed.sort_by(|x, y| (&x.kind, x.at).cmp(&(&y.kind, y.at)));
    for k in keyed {
        let Some(target) = lookup(&k.text)? else {
            return Err(RdyError::new(
                E_UNKNOWN_REF,
                format!(
                    "{} links to {} ({}), which no block holds as its key",
                    b.id, k.text, k.kind
                ),
                "use the target's id, or give the target that key",
            ));
        };
        let list = b.links.entry(k.kind).or_default();
        let at = k.at.min(list.len());
        list.insert(
            at,
            Link {
                target,
                part: k.part,
            },
        );
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    pack: String,
    scope: String,
}

/// The export file that names each block's author (per-block authorship). Optional on import.
pub const AUTHORS_FILE: &str = "authors.yaml";

/// One row of `authors.yaml`: who last wrote a block, from its producing change, and when it landed.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorYaml {
    id: String,
    actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run: Option<String>,
    role: String,
    at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    landed_ms: Option<i64>,
}

fn role_of(text: &str) -> Option<Role> {
    match text {
        "reader" => Some(Role::Reader),
        "writer" => Some(Role::Writer),
        "reviewer" => Some(Role::Reviewer),
        "approver" => Some(Role::Approver),
        _ => None,
    }
}

fn bad_authors(what: String) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("authors.yaml: {what}"),
        "export the project again, or delete authors.yaml to import every block as yours",
    )
}

/// How far after now an `at_ms` in `authors.yaml` may be: 24 h, for clocks that differ.
const AUTHORS_FUTURE_MS: i64 = 24 * 60 * 60 * 1000;

/// `authors.yaml` of export `dir`, checked against the import's blocks: by id. No file is no
/// rows.
fn read_authors(dir: &Path, blocks: &[Block]) -> Result<BTreeMap<BlockId, Provenance>, RdyError> {
    let path = dir.join(AUTHORS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(bad_authors(e.to_string())),
    };
    let rows: Vec<AuthorYaml> = serde_yaml_ng::from_str(&text).map_err(|e| {
        crate::did_you_mean::unknown_field_error(AUTHORS_FILE, &e.to_string())
            .unwrap_or_else(|| bad_authors(e.to_string()))
    })?;
    let known: BTreeSet<&BlockId> = blocks.iter().map(|b| &b.id).collect();
    let now = crate::now_ms();
    let mut out = BTreeMap::new();
    for row in rows {
        let id = BlockId::parse(&row.id)
            .map_err(|_| bad_authors(format!("{} is not a block id", row.id)))?;
        if !known.contains(&id) {
            return Err(bad_authors(format!("{id} is not a block of this export")));
        }
        if row.actor.trim().is_empty() {
            return Err(bad_authors(format!("{id} has a blank actor")));
        }
        if row.run.as_deref().is_some_and(|r| r.trim().is_empty()) {
            return Err(bad_authors(format!("{id} has a blank run")));
        }
        let role = role_of(&row.role).ok_or_else(|| {
            bad_authors(format!(
                "{id} has role {:?}; a role is reader, writer, reviewer or approver",
                row.role
            ))
        })?;
        // FW-HD1: a person may edit authors.yaml, so a time outside 0 .. now + 24 h is refused
        if row.at_ms < 0 || row.at_ms > now + AUTHORS_FUTURE_MS {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!(
                    "{AUTHORS_FILE}: {} has at_ms {}, outside 0 .. now + 24 h",
                    row.actor, row.at_ms
                ),
                "fix at_ms in authors.yaml (milliseconds since 1970)",
            ));
        }
        let prov = Provenance {
            actor: row.actor,
            run: row.run,
            role,
            at_ms: row.at_ms,
        };
        if out.insert(id.clone(), prov).is_some() {
            return Err(bad_authors(format!("{id} appears more than once")));
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, Serialize)]
pub struct ExportSummary {
    pub docs: usize,
    pub folders: usize,
    pub blocks: usize,
    pub blobs: usize,
    pub warnings: Vec<String>,
}

/// Writes the folders and docs under `parent` into `dir`: a folder is a directory holding
/// `_folder.yaml`, a doc is `<name>.yaml`. Neither file records the block's name or parent.
fn export_level(
    m: &Materialized,
    parent: Option<&BlockId>,
    level: usize,
    dir: &Path,
    summary: &mut ExportSummary,
    blobs: &mut BTreeMap<Hash, BlockId>,
    pack: &Pack,
) -> Result<(), RdyError> {
    // `level` folders are above this one; a damaged store may nest deeper than writes allow,
    // and what lies below the limit is left out (the caller reports the missing blocks)
    if level > tree::MAX_FOLDER_DEPTH {
        return Ok(());
    }
    let names = m.names_under(parent);
    let mut used: BTreeMap<String, &BlockId> = BTreeMap::new();
    for b in m.children(parent) {
        let Some(name) = names.get(&b.id) else {
            continue;
        };
        if let Some(other) = used.insert(name.to_ascii_lowercase(), &b.id) {
            return Err(RdyError::new(
                E_PATH_CLASH,
                format!(
                    "{other} and {} are both named {name:?} (ignoring case) under the same parent, so they cannot be exported as files",
                    b.id
                ),
                "rename one of them, then export again",
            ));
        }
        let mut head = to_yaml_struct(b);
        head.parent = None;
        head.fields.remove("name");
        summary.blocks += 1;
        if b.block_type == tree::FOLDER {
            let sub = dir.join(name);
            std::fs::create_dir_all(&sub).map_err(io_err)?;
            std::fs::write(
                sub.join(FOLDER_FILE),
                serde_yaml_ng::to_string(&head).expect("folder yaml"),
            )
            .map_err(io_err)?;
            summary.folders += 1;
            export_level(m, Some(&b.id), level + 1, &sub, summary, blobs, pack)?;
            continue;
        }
        if b.field_str("name").is_none() {
            summary.warnings.push(format!(
                "doc {} has no stored name: exported as {name}.yaml; importing it stores that name, so its version hash changes once",
                b.id
            ));
        }
        let mut list = vec![head];
        let mut stack = m.children(Some(&b.id));
        stack.reverse();
        while let Some(c) = stack.pop() {
            // a damaged store may loop: never list more blocks than there are
            if list.len() > m.len() {
                break;
            }
            list.push(to_yaml_struct(c));
            if pack.is_asset(&c.block_type)
                && let Some(h) = c.field_str("blob").and_then(|s| s.parse::<Hash>().ok())
            {
                blobs.entry(h).or_insert_with(|| c.id.clone());
            }
            let mut kids = m.children(Some(&c.id));
            kids.reverse();
            stack.extend(kids);
        }
        summary.blocks += list.len() - 1;
        summary.docs += 1;
        std::fs::create_dir_all(dir).map_err(io_err)?;
        std::fs::write(
            dir.join(format!("{name}.yaml")),
            serde_yaml_ng::to_string(&list).expect("doc yaml"),
        )
        .map_err(io_err)?;
    }
    Ok(())
}

/// A doc imported without a name gets the name readers already use, de-duplicated against names
/// under the same parent in the import and in the view.
fn assign_missing_names(blocks: &mut [Block], existing: &Materialized) {
    let mut taken: BTreeMap<Option<BlockId>, BTreeSet<String>> = BTreeMap::new();
    for b in blocks.iter().filter(|b| tree::is_tree_type(&b.block_type)) {
        let set = taken.entry(b.parent.clone()).or_insert_with(|| {
            existing
                .names_under(b.parent.as_ref())
                .into_values()
                .map(|n| n.to_ascii_lowercase())
                .collect()
        });
        if let Some(n) = b.field_str("name") {
            set.insert(n.to_ascii_lowercase());
        }
    }
    let mut nameless: Vec<usize> = (0..blocks.len())
        .filter(|&i| blocks[i].block_type == tree::DOC && blocks[i].field_str("name").is_none())
        .collect();
    nameless.sort_by(|&a, &b| blocks[a].id.cmp(&blocks[b].id));
    for i in nameless {
        let set = taken.entry(blocks[i].parent.clone()).or_default();
        let n = tree::dedupe(
            &tree::default_name(blocks[i].field_str("title"), &blocks[i].id),
            set,
        );
        set.insert(n.to_ascii_lowercase());
        blocks[i].fields.insert("name".into(), FieldValue::Str(n));
    }
}

/// Refuses an import whose docs or folders share a name (ignoring case) with each other or with
/// what the view already holds under the same parent: export would overwrite one with the other.
fn check_import_names(blocks: &[Block], existing: &Materialized) -> Result<(), RdyError> {
    let mut seen: BTreeMap<(Option<BlockId>, String), BlockId> = BTreeMap::new();
    for b in blocks.iter().filter(|b| tree::is_tree_type(&b.block_type)) {
        let Some(name) = b.field_str("name") else {
            continue;
        };
        let key = (b.parent.clone(), name.to_ascii_lowercase());
        let mut other = seen.insert(key.clone(), b.id.clone());
        if other.is_none() {
            other = existing
                .named(b.parent.as_ref(), &key.1)
                .into_iter()
                .find(|id| *id != b.id);
        }
        if let Some(other) = other {
            return Err(RdyError::new(
                E_PATH_CLASH,
                format!(
                    "{} and {other} are both named {name:?} (ignoring case) under the same parent",
                    b.id
                ),
                "rename one of them in the export files, or import into a fresh view",
            ));
        }
    }
    Ok(())
}

/// key1: refuses a key held twice (ignoring case) among the incoming blocks or between an
/// incoming block and another block of the view, then stores every key reference (and, lkey1,
/// every key link target) as its id.
fn resolve_import_keys(
    blocks: &mut [Block],
    keyed: Vec<Vec<KeyedLink>>,
    existing: &Materialized,
) -> Result<(), RdyError> {
    if let Some((k, a, b)) = crate::keys::clashes(blocks.iter()).first() {
        return Err(crate::keys::clash_error(k, a, b));
    }
    let incoming = Keys::of(blocks.iter());
    // the view's keys are indexed only when an incoming block has a key or a non-id reference
    let theirs: OnceCell<Keys> = OnceCell::new();
    let view_keys = || theirs.get_or_init(|| Keys::of(existing.iter()));
    for b in blocks.iter() {
        let Some(k) = crate::keys::key_of(b) else {
            continue;
        };
        if let Some((_, other)) = view_keys().holders(k).iter().find(|(_, o)| o != &b.id) {
            return Err(crate::keys::clash_error(k, &b.id, other));
        }
    }
    // a key the view holds twice (a floating view after another view landed it) is refused; a
    // view block the import replaces holds the key it brings, so its view key does not count
    let ids: BTreeSet<BlockId> = blocks.iter().map(|b| b.id.clone()).collect();
    let lookup = |t: &str| match incoming.get(t) {
        Some((_, id)) => Ok(Some(id.clone())),
        None => crate::keys::sole_holder(
            t,
            view_keys()
                .holders(t)
                .iter()
                .map(|(_, id)| id)
                .filter(|id| !ids.contains(*id)),
        ),
    };
    for (b, keyed) in blocks.iter_mut().zip(keyed) {
        place_keyed(b, keyed, &lookup)?;
        crate::keys::rewrite_block(b, &lookup)?;
    }
    Ok(())
}

/// akey1 (FW-PR3): gives each incoming doc and folder without a key the key derived from its
/// path, in (path, id) order, and returns them by block id. A path walks parents through the
/// incoming blocks, then the view. Taken: every incoming key, every key in the view held by a
/// block the import does not bring, the keys assigned so far, and the name of every other
/// top-level doc or folder after the import (a key that is another block's path would make that
/// path unreachable); all compared folded.
fn assign_import_keys(blocks: &mut [Block], existing: &Materialized) -> BTreeMap<String, String> {
    let wanted: Vec<usize> = (0..blocks.len())
        .filter(|&i| {
            tree::is_tree_type(&blocks[i].block_type) && !blocks[i].fields.contains_key("key")
        })
        .collect();
    if wanted.is_empty() {
        return BTreeMap::new();
    }
    let incoming: BTreeMap<&BlockId, &Block> = blocks.iter().map(|b| (&b.id, b)).collect();
    // each name on the chain, leaf first; None for a nameless doc or folder, a missing parent or
    // a chain too long to be a valid tree (the depth check refuses that import)
    let path_of = |id: &BlockId| -> Option<String> {
        let mut parts = vec![];
        let mut cur = Some(id.clone());
        for _ in 0..tree::MAX_FOLDER_DEPTH + 2 {
            let Some(c) = cur else {
                parts.reverse();
                return Some(parts.join("/"));
            };
            match incoming.get(&c) {
                Some(b) => {
                    if tree::is_tree_type(&b.block_type) {
                        parts.push(b.field_str("name")?.to_string());
                    }
                    cur = b.parent.clone();
                }
                None => {
                    let b = existing.get(&c)?;
                    if tree::is_tree_type(&b.block_type) {
                        parts.push(existing.name_of(&c)?);
                    }
                    cur = b.parent.clone();
                }
            }
        }
        None
    };
    let mut order: Vec<(String, BlockId, usize)> = wanted
        .into_iter()
        .filter_map(|i| Some((path_of(&blocks[i].id)?, blocks[i].id.clone(), i)))
        .collect();
    order.sort();
    let incoming_keys = Keys::of(blocks.iter());
    let ids: BTreeSet<BlockId> = blocks.iter().map(|b| b.id.clone()).collect();
    let view_keys = Keys::of(existing.iter());
    // folded top-level name → the docs and folders using it, as the view holds them after import
    let mut top: BTreeMap<String, BTreeSet<BlockId>> = BTreeMap::new();
    for b in blocks.iter().filter(|b| b.parent.is_none()) {
        if tree::is_tree_type(&b.block_type)
            && let Some(n) = b.field_str("name")
        {
            top.entry(rdy_model::key::fold(n))
                .or_default()
                .insert(b.id.clone());
        }
    }
    for b in existing.children(None) {
        if !ids.contains(&b.id)
            && let Some(n) = existing.name_of(&b.id)
        {
            top.entry(rdy_model::key::fold(&n))
                .or_default()
                .insert(b.id.clone());
        }
    }
    let mut assigned: BTreeSet<String> = BTreeSet::new();
    let mut out = BTreeMap::new();
    for (path, id, i) in order {
        let k = crate::keys::auto_key(&path, &blocks[i].block_type, &|c| {
            incoming_keys.get(c).is_some()
                || view_keys.holders(c).iter().any(|(_, h)| !ids.contains(h))
                || assigned.contains(&rdy_model::key::fold(c))
                || top
                    .get(&rdy_model::key::fold(c))
                    .is_some_and(|holders| holders.iter().any(|h| h != &id))
        });
        assigned.insert(rdy_model::key::fold(&k));
        blocks[i]
            .fields
            .insert("key".into(), FieldValue::Str(k.clone()));
        blocks[i]
            .fields
            .insert(rdy_model::key::AUTO_FIELD.into(), FieldValue::Bool(true));
        out.insert(id.to_string(), k);
    }
    out
}

/// skel1: the required sections each imported doc lacks among the imported sections under it.
fn import_skeleton_gaps(pack: &rdy_schema::Pack, blocks: &[Block]) -> Vec<rdy_schema::Issue> {
    let titles = crate::keys::section_titles(blocks);
    blocks
        .iter()
        .filter(|b| b.block_type == tree::DOC)
        .flat_map(|d| {
            skeleton_gaps(
                pack,
                d,
                titles.get(&d.id).map(Vec::as_slice).unwrap_or_default(),
            )
        })
        .collect()
}

fn io_err(e: std::io::Error) -> RdyError {
    RdyError::new(E_IO, e.to_string(), "check the path and permissions")
}

pub(crate) fn to_yaml_struct(b: &Block) -> BlockYaml {
    BlockYaml {
        id: b.id.clone(),
        block_type: b.block_type.clone(),
        parent: b.parent.clone(),
        order: b.order.clone(),
        fields: b
            .fields
            .iter()
            .map(|(k, v)| (k.clone(), v.to_yaml()))
            .collect(),
        body: b.body.clone(),
        links: b
            .links
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.iter()
                        .map(|l| LinkYaml {
                            target: l.target.to_string(),
                            part: l.part.clone(),
                        })
                        .collect(),
                )
            })
            .collect(),
        pins: b
            .pins
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect(),
    }
}

/// A block read from YAML, and its link targets written as keys (lkey1). A target that parses as
/// a block id is a link; else a text that is a well-formed key is kept to be resolved; else the
/// YAML is refused.
pub(crate) fn from_yaml_struct(y: BlockYaml) -> Result<(Block, Vec<KeyedLink>), RdyError> {
    let bad = |m: String| RdyError::new(E_BAD_INPUT, m, "fix the YAML and retry");
    let mut keyed = vec![];
    let mut links = BTreeMap::new();
    for (kind, list) in y.links {
        let mut out = Vec::with_capacity(list.len());
        for (at, l) in list.into_iter().enumerate() {
            if let Ok(target) = BlockId::parse(&l.target) {
                out.push(Link {
                    target,
                    part: l.part,
                });
            } else if rdy_model::key::problem(&l.target).is_none() {
                keyed.push(KeyedLink {
                    kind: kind.clone(),
                    at,
                    text: l.target,
                    part: l.part,
                });
            } else {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!(
                        "link target {:?} in {kind} is neither a block id nor a key",
                        l.target
                    ),
                    "write the target's id or its key",
                ));
            }
        }
        links.insert(kind, out);
    }
    let block = Block {
        id: y.id,
        block_type: y.block_type,
        parent: y.parent,
        order: y.order,
        fields: y
            .fields
            .iter()
            .map(|(k, v)| {
                Ok((
                    k.clone(),
                    FieldValue::from_yaml(v).map_err(|e| bad(format!("field {k}: {e}")))?,
                ))
            })
            .collect::<Result<_, RdyError>>()?,
        body: y.body,
        links,
        pins: y
            .pins
            .into_iter()
            .map(|(k, v)| Ok((k, v.parse::<Hash>().map_err(|e| bad(e.to_string()))?)))
            .collect::<Result<_, RdyError>>()?,
        read_identity: None,
    };
    Ok((block, keyed))
}

/// Dotfiles (e.g. `.DS_Store`) are never part of an export.
fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

/// sym1: import never follows a symbolic link out of the export.
fn refuse_symlink(path: &Path) -> Result<(), RdyError> {
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(RdyError::new(
            E_FILE_FORMAT,
            format!(
                "{} is a symbolic link; rdy does not follow links in an export",
                path.display()
            ),
            "replace the link with the file it points to",
        ));
    }
    Ok(())
}

pub fn block_to_yaml(b: &Block) -> String {
    serde_yaml_ng::to_string(&to_yaml_struct(b)).expect("block yaml")
}

/// A block read from YAML whose link targets are ids; a key as a link target needs the keys of a
/// view, so it is refused here (`Rdy::edit_yaml` resolves it).
pub fn block_from_yaml(text: &str) -> Result<Block, RdyError> {
    let (block, keyed) = parse_yaml(text)?;
    if let Some(k) = keyed.first() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!(
                "link target {:?} in {} is a key, which needs a view to resolve it",
                k.text, k.kind
            ),
            "write the target's id, or edit through a view",
        ));
    }
    Ok(block)
}

/// A YAML syntax error is `E_BAD_INPUT`; a key the block does not take is `E_FILE_FORMAT` with a
/// did-you-mean, as import and commit report it (hd8, ruling Q14).
fn parse_yaml(text: &str) -> Result<(Block, Vec<KeyedLink>), RdyError> {
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(text)
        .map_err(|e| RdyError::new(E_BAD_INPUT, e.to_string(), "fix the YAML and retry"))?;
    if let Some(f) = tree_io::key_faults(&value).into_iter().next() {
        let line = crate::yaml_pos::positions(text)
            .ok()
            .and_then(|p| p.get(&f.segs()).map(|at| at.0))
            .unwrap_or(1);
        return Err(RdyError::new(
            E_FILE_FORMAT,
            format!(
                "line {line}: unknown key \"{}\" in {}",
                f.key,
                f.place("the block")
            ),
            f.fix(),
        ));
    }
    let y: BlockYaml = serde_yaml_ng::from_str(text)
        .map_err(|e| RdyError::new(E_BAD_INPUT, e.to_string(), "fix the YAML and retry"))?;
    from_yaml_struct(y)
}

/// insc1: an export (or a rendered site) is a state someone can read back, so never `inflight`.
pub fn refuse_inflight_export(scope: &Scope) -> Result<(), RdyError> {
    if *scope == Scope::Inflight {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "inflight is an analysis scope: export (render) canon, a view or a version",
            "export or render canon, a view (view:<name>) or a version (tag:<version>)",
        ));
    }
    Ok(())
}

impl ReadState {
    pub fn export(&self, scope: &Scope, out: &Path) -> Result<ExportSummary, RdyError> {
        refuse_inflight_export(scope)?;
        if out.exists() && std::fs::read_dir(out).map_err(io_err)?.next().is_some() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("{} is not empty", out.display()),
                "choose an empty directory (or one that does not exist yet) so no stale files mix into the export",
            ));
        }
        let name = out.file_name().ok_or_else(|| {
            RdyError::new(
                E_BAD_INPUT,
                format!("{} does not name a directory", out.display()),
                "pass a directory path such as ./export",
            )
        })?;
        let parent = out
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).map_err(io_err)?;
        // everything is written to a staging directory beside `out` and renamed into place at
        // the end: a failure while writing leaves nothing behind; if the final rename itself
        // fails, the staging directory is cleared by the next export
        let staging = parent.join(format!(".{}.rdy-export-partial", name.to_string_lossy()));
        if staging.exists() {
            std::fs::remove_dir_all(&staging).map_err(io_err)?;
        }
        let summary = match self.write_export(scope, &staging) {
            Ok(summary) => summary,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(e);
            }
        };
        // POSIX rename atomically replaces an empty directory
        std::fs::rename(&staging, out).map_err(io_err)?;
        Ok(summary)
    }

    fn write_export(&self, scope: &Scope, out: &Path) -> Result<ExportSummary, RdyError> {
        let m = self.materialized(scope)?;
        std::fs::create_dir_all(out.join("docs")).map_err(io_err)?;
        std::fs::create_dir_all(out.join("blobs")).map_err(io_err)?;
        let mut summary = ExportSummary {
            docs: 0,
            folders: 0,
            blocks: 0,
            blobs: 0,
            warnings: vec![],
        };
        // each blob, with the first figure that names it
        let mut blobs = BTreeMap::new();
        export_level(
            &m,
            None,
            0,
            &out.join("docs"),
            &mut summary,
            &mut blobs,
            self.pack(),
        )?;
        if summary.blocks != m.len() {
            let missing = m.len() - summary.blocks.min(m.len());
            return Err(RdyError::new(
                E_IO,
                format!(
                    "{missing} blocks are not inside any folder or doc reachable from the top level (or lie more than {} folders deep), so they cannot be exported",
                    tree::MAX_FOLDER_DEPTH
                ),
                "run rdy fsck; move or delete those blocks, then export again",
            ));
        }
        for (h, figure) in &blobs {
            let not_found = || {
                RdyError::new(
                    E_NOT_FOUND,
                    format!("{figure} names blob {h}, which is not a blob in the store"),
                    format!(
                        "attach the asset again (rdy_asset) and point {figure} at it, or delete {figure}"
                    ),
                )
            };
            if !self.reader().has_blob(h)? {
                return Err(not_found());
            }
            let bytes = self.reader().get_blob(h).map_err(|e| match e {
                rdy_store::StoreError::NotFound(_) => not_found(),
                other => other.into(),
            })?;
            std::fs::write(out.join("blobs").join(h.to_string()), bytes).map_err(io_err)?;
            summary.blobs += 1;
        }
        let scope_name = match scope {
            Scope::Canon => "canon".to_string(),
            Scope::View(v) if v.contains(':') => format!("view:{v}"),
            Scope::View(v) => v.clone(),
            Scope::At(h) => format!("landing:{h}"),
            Scope::Proposal(v) => format!("proposal:{v}"),
            Scope::Inflight => crate::INFLIGHT_SCOPE.to_string(),
        };
        // per-block authorship: who last wrote each block, and when it landed; an in-flight
        // scope has no one chain, so it carries none
        if !matches!(scope, Scope::Inflight) {
            let ids: Vec<BlockId> = m.iter().map(|b| b.id.clone()).collect();
            let rows: Vec<AuthorYaml> = self
                .last_changes(scope, &ids)?
                .into_iter()
                .map(|(id, h)| AuthorYaml {
                    id: id.to_string(),
                    actor: h.author.actor,
                    run: h.author.run,
                    role: h.author.role,
                    at_ms: h.author.at_ms,
                    landed_ms: h.landing.map(|l| l.at_ms),
                })
                .collect();
            std::fs::write(
                out.join(AUTHORS_FILE),
                serde_yaml_ng::to_string(&rows).expect("authors"),
            )
            .map_err(io_err)?;
        }
        let manifest = Manifest {
            format: 3,
            pack: self.pack().name.clone(),
            scope: scope_name,
        };
        std::fs::write(
            out.join("rdy-export.yaml"),
            serde_yaml_ng::to_string(&manifest).expect("manifest"),
        )
        .map_err(io_err)?;
        Ok(summary)
    }
}

/// The label an import's messages carry: the final component of the directory's canonical
/// path. A directory with no final component, or a name that is not UTF-8, has no label.
fn import_label(dir: &Path) -> Option<String> {
    let canon = std::fs::canonicalize(dir).ok()?;
    canon.file_name()?.to_str().map(str::to_string)
}

impl Rdy {
    pub fn import(&mut self, s: &Session, view: &str, dir: &Path) -> Result<WriteResult, RdyError> {
        if !s.role.can_land() {
            return Err(RdyError::new(
                E_PERMISSION,
                "import can carry accepted decisions, so it needs an approver",
                "run the import as an approver session",
            ));
        }
        crate::write::check_view_access(&self.read(), view, s, crate::views::Action::Commit)?;
        // sym1: the export's own root entries are never followed either
        for root in ["rdy-export.yaml", "docs", "blobs", AUTHORS_FILE] {
            refuse_symlink(&dir.join(root))?;
        }
        let manifest_path = dir.join("rdy-export.yaml");
        let manifest_text = std::fs::read_to_string(&manifest_path).map_err(|e| {
            RdyError::new(
                E_BAD_INPUT,
                format!("{}: {e}", manifest_path.display()),
                "point import at a directory made by rdy export",
            )
        })?;
        let manifest: Manifest = serde_yaml_ng::from_str(&manifest_text).map_err(|e| {
            crate::did_you_mean::unknown_field_error("rdy-export.yaml", &e.to_string())
                .unwrap_or_else(|| {
                    RdyError::new(
                        E_BAD_INPUT,
                        format!("rdy-export.yaml: {e}"),
                        "re-export the project",
                    )
                })
        })?;
        // lpin1: format 3 pins hold content hashes; import recomputes every pin, so a format-2
        // tree reads the same way
        if !(2..=3).contains(&manifest.format) {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("unsupported export format {}", manifest.format),
                "this rdy reads export formats 2 and 3; re-export with this rdy",
            ));
        }
        if !self.lineage.contains(&manifest.pack) {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!(
                    "the export was made with pack {:?}; this project uses {:?}, which does not extend it",
                    manifest.pack, self.pack.name
                ),
                format!(
                    "import into a project whose pack is {} or extends it",
                    manifest.pack
                ),
            ));
        }
        let blob_dir = dir.join("blobs");
        let mut blob_bytes = vec![];
        let mut imported_blobs = BTreeSet::new();
        if blob_dir.exists() {
            for entry in std::fs::read_dir(&blob_dir).map_err(io_err)? {
                let path = entry.map_err(io_err)?.path();
                refuse_symlink(&path)?;
                if !path.is_file() || is_hidden(&path) {
                    continue;
                }
                let bytes = std::fs::read(&path).map_err(io_err)?;
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                let got = rdy_store::Store::blob_hash(&bytes);
                if got.to_string() != name {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        format!("blob {name} hashes to {got}"),
                        "the export is corrupt; export it again",
                    ));
                }
                imported_blobs.insert(got);
                blob_bytes.push(bytes);
            }
        }
        let (mut blocks, keyed): (Vec<Block>, Vec<Vec<KeyedLink>>) = {
            let ignore = crate::wc::ignore::Ignore::load(dir)?;
            let (loaded, diags) = tree_io::read_tree_with(&dir.join("docs"), "docs", Some(&ignore));
            if let Some(d) = diags
                .iter()
                .find(|d| d.severity == rdy_schema::Severity::Error)
            {
                return Err(d.to_error());
            }
            loaded.into_iter().map(|l| (l.block, l.keyed)).unzip()
        };
        let scope = Scope::View(view.to_string());
        // an import is a commit: it claims an unowned view and checks against the view's
        // recorded conflicts, as `write_linked` does
        let st = self.read();
        let claim = crate::views::commit_claim(&st, s, view)?;
        let conflicts = st.conflicts(&scope)?;
        let existing = st.materialized(&scope)?;
        drop(st);
        // duplicates first: a duplicated doc would otherwise read as a name clash with itself
        let mut ids = BTreeSet::new();
        for b in &blocks {
            if !ids.insert(&b.id) {
                return Err(RdyError::new(
                    E_ALREADY_EXISTS,
                    format!("block {} appears more than once in the import", b.id),
                    "remove the duplicate from the export files",
                ));
            }
        }
        drop(ids);
        let authors = read_authors(dir, &blocks)?;
        assign_missing_names(&mut blocks, &existing);
        check_import_names(&blocks, &existing)?;
        resolve_import_keys(&mut blocks, keyed, &existing)?;
        let keys = assign_import_keys(&mut blocks, &existing);
        let incoming: BTreeMap<BlockId, &Block> =
            blocks.iter().map(|b| (b.id.clone(), b)).collect();
        let parent_of = |id: &BlockId| -> Option<Option<BlockId>> {
            incoming
                .get(id)
                .map(|b| b.parent.clone())
                .or_else(|| existing.get(id).map(|b| b.parent.clone()))
        };
        for b in &blocks {
            if let Some(p) = &b.parent
                && parent_of(p).is_none()
            {
                return Err(RdyError::new(
                    E_UNKNOWN_BLOCK,
                    format!(
                        "{} has parent {p}, which is in neither the import nor the view",
                        b.id
                    ),
                    "export the whole project",
                ));
            }
            let mut seen = BTreeSet::from([b.id.clone()]);
            let mut folders = usize::from(b.block_type == tree::FOLDER);
            let mut cur = b.parent.clone();
            while let Some(c) = cur {
                if !seen.insert(c.clone()) {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        format!("{} is part of a parent cycle", b.id),
                        "fix the parent fields in the export",
                    ));
                }
                let is_folder = incoming
                    .get(&c)
                    .copied()
                    .or_else(|| existing.get(&c))
                    .is_some_and(|p| p.block_type == tree::FOLDER);
                folders += usize::from(is_folder);
                if folders > tree::MAX_FOLDER_DEPTH {
                    break;
                }
                cur = parent_of(&c).flatten();
            }
            if folders > tree::MAX_FOLDER_DEPTH {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!(
                        "{} sits {folders} folders deep (at most {} allowed)",
                        b.id,
                        tree::MAX_FOLDER_DEPTH
                    ),
                    format!(
                        "keep folders at most {} deep: move it higher in the export",
                        tree::MAX_FOLDER_DEPTH
                    ),
                ));
            }
        }
        let mut issues = vec![];
        for b in &blocks {
            if existing.get(&b.id).is_some() {
                return Err(RdyError::new(
                    E_ALREADY_EXISTS,
                    format!("block {} already exists in view {view}", b.id),
                    "import into a fresh project or view",
                ));
            }
            // msw1 (#1160): a create/create conflict has no base, so the view reads the block as
            // absent; importing it would silently resolve the conflict with the import's version
            if let Some(c) = conflicts.iter().find(|c| c.block == b.id) {
                return Err(RdyError::new(
                    E_ALREADY_EXISTS,
                    format!(
                        "block {} already exists in view {view}: it is conflicted ({})",
                        b.id,
                        c.id.short()
                    ),
                    "import into a fresh project or view",
                ));
            }
            let exists = |id: &BlockId| incoming.contains_key(id) || existing.get(id).is_some();
            for t in b.ref_tokens() {
                match BlockId::parse(&t) {
                    Ok(id) if exists(&id) => {}
                    _ => {
                        return Err(RdyError::new(
                            E_UNKNOWN_REF,
                            format!(
                                "{} references {{{{{t}}}}}, which is not in the import",
                                b.id
                            ),
                            format!(
                                "export the whole project, or remove the reference; {}",
                                crate::keys::literal_hint(&t)
                            ),
                        ));
                    }
                }
            }
            for l in b.links.values().flatten() {
                if !exists(&l.target) {
                    return Err(RdyError::new(
                        E_UNKNOWN_REF,
                        format!("{} links to {}, which is not in the import", b.id, l.target),
                        "export the whole project",
                    ));
                }
            }
            // blob2: an asset block's blob must be in the export's blobs/ or a blob in this
            // project; non-asset types may have a field named blob that is not an asset
            if self.pack.is_asset(&b.block_type)
                && let Some(v) = b.fields.get("blob")
            {
                let known = match v.as_str().and_then(|s| s.parse::<Hash>().ok()) {
                    Some(h) => imported_blobs.contains(&h) || self.store.has_blob(&h)?,
                    None => false,
                };
                if !known {
                    return Err(RdyError::new(
                        E_UNKNOWN_REF,
                        format!(
                            "{} names blob {}, which is neither in the export's blobs/ nor a blob in this project",
                            b.id,
                            v.as_str().unwrap_or("(not a string)")
                        ),
                        "export the whole project again: rdy export copies every figure's blob",
                    ));
                }
            }
            let parent = b
                .parent
                .as_ref()
                .and_then(|p| incoming.get(p).copied().or_else(|| existing.get(p)));
            issues.extend(validate_prefix(&self.pack, b));
            issues.extend(validate_block(&self.pack, b, parent));
        }
        if has_errors(&issues) {
            let mut e = RdyError::new(
                E_VALIDATION,
                "the import does not validate against this project's pack",
                "fix the listed blocks or use a compatible pack",
            );
            e.issues = issues;
            return Err(e);
        }
        // lpin1: every pin is recomputed from the imported content, as a write's create pins
        // it: each cited target (reference or link) to its content hash, the incoming block
        // with that id first, else the view's. A block is never pinned to itself. Pins in the
        // files are ignored.
        let tracking = self.store.tracking();
        let content = |id: &BlockId| {
            incoming
                .get(id)
                .copied()
                .or_else(|| existing.get(id))
                .map(|t| t.content_hash_with(tracking))
        };
        let pins: Vec<BTreeMap<BlockId, Hash>> = blocks
            .iter()
            .map(|b| {
                let refs = b.ref_tokens();
                let refs = refs.iter().filter_map(|t| BlockId::parse(t).ok());
                let links = b.links.values().flatten().map(|l| l.target.clone());
                refs.chain(links)
                    .filter(|t| *t != b.id)
                    .filter_map(|t| content(&t).map(|h| (t, h)))
                    .collect()
            })
            .collect();
        for (b, p) in blocks.iter_mut().zip(pins) {
            b.pins = p;
        }
        // blobs go in only once the import is known to be valid, so a refused import stores nothing
        for bytes in &blob_bytes {
            self.store.put_blob(bytes)?;
        }
        let warnings = import_skeleton_gaps(&self.pack, &blocks);
        // one change per author (grouped by actor), oldest first; blocks with no author row are
        // the importer's, last. Each change takes the run, role and time of its newest block
        // (largest at_ms, then id).
        struct Group {
            /// The newest block: its time, then its id.
            newest: (i64, BlockId),
            provenance: Provenance,
            ops: Vec<Op>,
        }
        let mut groups: BTreeMap<String, Group> = BTreeMap::new();
        let mut mine: Vec<Op> = vec![];
        for block in blocks {
            match authors.get(&block.id) {
                Some(p) => {
                    let newest = (p.at_ms, block.id.clone());
                    let g = groups.entry(p.actor.clone()).or_insert_with(|| Group {
                        newest: newest.clone(),
                        provenance: p.clone(),
                        ops: vec![],
                    });
                    if newest > g.newest {
                        g.newest = newest;
                        g.provenance = p.clone();
                    }
                    g.ops.push(Op::Create { block });
                }
                None => mine.push(Op::Create { block }),
            }
        }
        let mut order: Vec<(Provenance, Vec<Op>)> = groups
            .into_values()
            .map(|g| (g.provenance, g.ops))
            .collect();
        order.sort_by(|a, b| (a.0.at_ms, &a.0.actor).cmp(&(b.0.at_ms, &b.0.actor)));
        // df6: the messages name the directory, never its path (a path can hold a user name)
        let from = import_label(dir)
            .map(|l| format!(" from {l}"))
            .unwrap_or_default();
        let mut planned: Vec<(Provenance, Vec<Op>, String)> = order
            .into_iter()
            .map(|(p, ops)| {
                let message = format!(
                    "import{}: {} block(s) by {}, imported by {}",
                    from,
                    ops.len(),
                    p.actor,
                    s.actor
                );
                (p, ops, message)
            })
            .collect();
        // an export with no blocks still writes the importer's (empty) change, as before
        if !mine.is_empty() || planned.is_empty() {
            planned.push((s.provenance(), mine, format!("import{from}")));
        }
        // one write for every change (all or nothing). Each change keeps its author's
        // provenance; the view ops (the implicit create, a claim, the commits) and the view's
        // last write are the importer's, so the view stays the importer's, `rdy undo` by the
        // importer finds the import, and the view is not idle after old author times. Every
        // change stores the importer (FW-F7b).
        let importer = ActorRun {
            actor: s.actor.clone(),
            run: s.run.clone(),
        };
        let mut all_ops = vec![];
        let changes: Vec<Change> = planned
            .into_iter()
            .map(|(provenance, ops, message)| {
                all_ops.extend(ops.iter().cloned());
                Change {
                    view: view.into(),
                    ops,
                    deps: vec![],
                    provenance,
                    justification: None,
                    message,
                    links: ChangeLinks {
                        imported_by: Some(importer.clone()),
                        ..ChangeLinks::default()
                    },
                    read_identity: None,
                }
            })
            .collect();
        let committed = self.store.commit_many(
            view,
            changes,
            &|id| existing.version_of(id),
            &conflicts,
            claim,
            &s.provenance(),
        )?;
        // one change carries the build forward as a commit; several are read again
        match committed.as_slice() {
            [one] => self.publish(crate::Delta::Commit {
                view: view.to_string(),
                change: *one,
                ops: all_ops.clone(),
            })?,
            _ => self.publish(crate::Delta::View {
                view: view.to_string(),
            })?,
        }
        let ids: Vec<String> = committed.iter().map(ToString::to_string).collect();
        let created = all_ops
            .iter()
            .enumerate()
            .map(|(i, op)| (format!("op{i}"), op.block_id().to_string()))
            .collect();
        Ok(WriteResult {
            change: ids.last().cloned().unwrap_or_default(),
            changes: ids,
            created,
            names: BTreeMap::new(),
            keys,
            inherited: BTreeMap::new(),
            warnings,
        })
    }

    /// lkey1: `edit_block_with` for a block written as YAML, a key as a link target resolved
    /// with the keys of the view. `reason` is the edit's editorial reason (`rdy edit --reason`).
    pub fn edit_yaml(
        &mut self,
        s: &Session,
        view: &str,
        yaml: &str,
        brief: Option<String>,
        reason: Option<&str>,
    ) -> Result<WriteResult, RdyError> {
        self.edit_yaml_at(s, view, yaml, None, brief, reason)
    }

    /// msw8 (#1064): [`Rdy::edit_yaml`] against `base`, the version the editor read (the
    /// `block` op's `version`). A block that changed since then is refused (`E_STALE_BASE`, or
    /// `E_BRIEF_STALE` for a stable block), so a stale buffer never reverts a concurrent change.
    pub fn edit_yaml_at(
        &mut self,
        s: &Session,
        view: &str,
        yaml: &str,
        base: Option<EditBase<'_>>,
        brief: Option<String>,
        reason: Option<&str>,
    ) -> Result<WriteResult, RdyError> {
        let (mut edited, keyed) = parse_yaml(yaml)?;
        if !keyed.is_empty() {
            let m = self.read().materialized(&Scope::View(view.to_string()))?;
            let keys = Keys::of(m.iter());
            // a key two blocks of the view hold is refused, as in a write
            place_keyed(&mut edited, keyed, &|t| {
                crate::keys::sole_holder(t, keys.holders(t).iter().map(|(_, id)| id))
            })?;
        }
        self.edit_block_at(s, view, edited, base, brief, reason)
    }

    /// [`Rdy::edit_block_with`] with no reason.
    pub fn edit_block(
        &mut self,
        s: &Session,
        view: &str,
        edited: Block,
        brief: Option<String>,
    ) -> Result<WriteResult, RdyError> {
        self.edit_block_with(s, view, edited, brief, None)
    }

    /// Writes `edited` over its current version in `view`: fields, body and links. A non-blank
    /// `reason` goes with the write as an editorial `clarify` justification, which a tracking
    /// edit of an accepted or rejected decision needs (trk1); refused without one, the fix names
    /// `rdy edit --reason`.
    pub fn edit_block_with(
        &mut self,
        s: &Session,
        view: &str,
        edited: Block,
        brief: Option<String>,
        reason: Option<&str>,
    ) -> Result<WriteResult, RdyError> {
        self.edit_block_at(s, view, edited, None, brief, reason)
    }

    /// msw8 (#1064): [`Rdy::edit_block_with`] against `base`, the version hash the editor read;
    /// `None` is the current version. When `base` is not the current version, the local checks
    /// are skipped (they compare against a version the editor never saw) and the write decides:
    /// `E_BRIEF_STALE` for a stable block, else `E_STALE_BASE`.
    pub fn edit_block_at(
        &mut self,
        s: &Session,
        view: &str,
        edited: Block,
        base: Option<EditBase<'_>>,
        brief: Option<String>,
        reason: Option<&str>,
    ) -> Result<WriteResult, RdyError> {
        let scope = Scope::View(view.to_string());
        let cur = self.read().block(&scope, &edited.id)?;
        let cur_hash = cur.version_hash().to_string();
        let token = base.is_some();
        let conflict = base.and_then(|b| b.conflict);
        let base = base.map_or(cur_hash.as_str(), |b| b.version).to_string();
        let current = base == cur_hash;
        let id = &edited.id;
        let now = self.read().conflict_on(&scope, id)?.map(|c| c.id);
        // msw8 fix round 1: a stale buffer never resolves a conflict; the view reads the
        // conflict's base, and a side may be the editor's own old version, which cres1 accepts
        if let (false, Some(n)) = (current, now) {
            // the edit token is the base and the conflict: a retry needs both
            return Err(RdyError::new(
                E_STALE_BASE,
                format!(
                    "{id} changed since you read it: your base is {}, current is {}",
                    short(&base),
                    short(&cur_hash)
                ),
                format!(
                    "re-read {id} (rdy_context lists its conflict's sides) and retry with base={cur_hash} and conflict={n}"
                ),
            ));
        }
        // msw8 fix round 1 (ruling): the edit token carries the conflict the editor read; a
        // conflict that appeared, re-keyed or went away since then is a change the editor never saw
        if token && conflict != now.map(|c| c.to_string()).as_deref() {
            let message = match (conflict, now) {
                (None, n) => format!(
                    "{id} changed since you read it: it is now conflicted ({})",
                    n.map(|n| n.short()).unwrap_or_default()
                ),
                (Some(r), None) => format!(
                    "{id} changed since you read it: its conflict {} is resolved",
                    short(r)
                ),
                (Some(r), Some(n)) => format!(
                    "{id} changed since you read it: its conflict {} is now {}",
                    short(r),
                    n.short()
                ),
            };
            return Err(RdyError::new(
                E_STALE_BASE,
                message,
                format!("re-read {id} (rdy_context lists its conflict's sides) and edit it again"),
            ));
        }
        // the diff is taken against the version the editor read, so a field or body the buffer
        // left alone is never sent as a change (the write refuses a stale base before it applies)
        let cur = if current {
            cur
        } else {
            match base.parse::<Hash>() {
                Ok(h) => self.store.get_block(&h).unwrap_or(cur),
                Err(_) => cur,
            }
        };
        if current && cur == edited {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("nothing changed in {}", edited.id),
                "change a field, the body or the links, or cancel the edit",
            ));
        }
        // edit1: pins are rdy's record of what a block was checked against; an edit carries
        // fields, body and links only, so a pin change would commit an update that changes nothing
        if current && cur.pins != edited.pins {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("the edit changes the pins of {}", edited.id),
                "pins are set by rdy; remove the pins change",
            ));
        }
        if current
            && (cur.block_type != edited.block_type
                || cur.parent != edited.parent
                || cur.order != edited.order)
        {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "type, parent and order cannot be changed by edit",
                "use a move op to change parent or order",
            ));
        }
        let fields = edited
            .fields
            .iter()
            .filter(|(k, v)| cur.fields.get(*k) != Some(*v))
            .map(|(k, v)| (k.clone(), field_json(v)))
            .collect();
        let unset = cur
            .fields
            .keys()
            .filter(|k| !edited.fields.contains_key(*k))
            .cloned()
            .collect();
        let links = (cur.links != edited.links).then(|| {
            let mut m: BTreeMap<String, Vec<LinkInput>> = edited
                .links
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.iter()
                            .map(|l| LinkInput {
                                target: l.target.to_string(),
                                part: l.part.clone(),
                            })
                            .collect(),
                    )
                })
                .collect();
            // link1: an update merges link kinds, so a kind the edit removed is sent as []
            for k in cur.links.keys() {
                m.entry(k.clone()).or_default();
            }
            m
        });
        let op = OpInput::Update {
            id: edited.id.to_string(),
            base: Some(base),
            fields,
            unset,
            body: (cur.body != edited.body).then(|| edited.body.clone()),
            links,
            block_type: None,
            move_to: None,
        };
        let reason = reason.filter(|r| !r.trim().is_empty());
        let justification = reason.map(|r| Justification::Editorial {
            class: EditorialClass::Clarify,
            reason: r.to_string(),
        });
        self.write_briefed(
            s,
            WriteRequest {
                view: view.into(),
                ops: vec![op],
                justification,
                message: format!("edit {}", edited.id),
            },
            brief,
        )
        .map_err(|mut e| {
            if e.code == E_BAD_INPUT && e.message == TRACKING_NEEDS_REASON {
                e.fix = format!(
                    "give the edit a reason: rdy edit {} --reason \"<why>\"",
                    edited.id
                );
            }
            e
        })
    }
}

/// msw8: what an editor read of a block, sent back with its edit: the block's version (the `block`
/// op's `version`) and the conflict the view read it under (`conflict`; none when not
/// conflicted). A block or conflict that changed since then refuses the edit (`E_STALE_BASE`).
#[derive(Clone, Copy, Debug)]
pub struct EditBase<'a> {
    pub version: &'a str,
    pub conflict: Option<&'a str>,
}

/// The first 12 characters of a version or conflict hash the caller sent, as `Hash::short`.
fn short(h: &str) -> &str {
    h.get(..12).unwrap_or(h)
}
