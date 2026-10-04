use crate::error::*;
use crate::query::field_json;
use crate::write::{LinkInput, OpInput, WriteRequest, WriteResult};
use crate::{Rdy, Scope, Session};
use rdy_model::{Block, BlockId, Change, FieldValue, Hash, Link, Op};
use rdy_schema::{has_errors, validate_block};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct LinkYaml {
    target: BlockId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    part: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct BlockYaml {
    id: BlockId,
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<BlockId>,
    order: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    fields: BTreeMap<String, serde_yaml_ng::Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    body: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    links: BTreeMap<String, Vec<LinkYaml>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pins: BTreeMap<BlockId, String>,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format: u32,
    pack: String,
    scope: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExportSummary {
    pub docs: usize,
    pub blocks: usize,
    pub blobs: usize,
}

fn io_err(e: std::io::Error) -> RdyError {
    RdyError::new(E_IO, e.to_string(), "check the path and permissions")
}

fn to_yaml_struct(b: &Block) -> BlockYaml {
    BlockYaml {
        id: b.id.clone(),
        block_type: b.block_type.clone(),
        parent: b.parent.clone(),
        order: b.order.clone(),
        fields: b.fields.iter().map(|(k, v)| (k.clone(), v.to_yaml())).collect(),
        body: b.body.clone(),
        links: b.links.iter().map(|(k, v)| (k.clone(), v.iter().map(|l| LinkYaml { target: l.target.clone(), part: l.part.clone() }).collect())).collect(),
        pins: b.pins.iter().map(|(k, v)| (k.clone(), v.to_string())).collect(),
    }
}

fn from_yaml_struct(y: BlockYaml) -> Result<Block, RdyError> {
    let bad = |m: String| RdyError::new(E_BAD_INPUT, m, "fix the YAML and retry");
    Ok(Block {
        id: y.id,
        block_type: y.block_type,
        parent: y.parent,
        order: y.order,
        fields: y.fields.iter().map(|(k, v)| Ok((k.clone(), FieldValue::from_yaml(v).map_err(|e| bad(format!("field {k}: {e}")))?))).collect::<Result<_, RdyError>>()?,
        body: y.body,
        links: y.links.into_iter().map(|(k, v)| (k, v.into_iter().map(|l| Link { target: l.target, part: l.part }).collect())).collect(),
        pins: y.pins.into_iter().map(|(k, v)| Ok((k, v.parse::<Hash>().map_err(|e| bad(e.to_string()))?))).collect::<Result<_, RdyError>>()?,
    })
}

pub fn block_to_yaml(b: &Block) -> String {
    serde_yaml_ng::to_string(&to_yaml_struct(b)).expect("block yaml")
}

pub fn block_from_yaml(text: &str) -> Result<Block, RdyError> {
    let y: BlockYaml = serde_yaml_ng::from_str(text).map_err(|e| RdyError::new(E_BAD_INPUT, e.to_string(), "fix the YAML and retry"))?;
    from_yaml_struct(y)
}

impl Rdy {
    pub fn export(&mut self, scope: &Scope, out: &Path) -> Result<ExportSummary, RdyError> {
        let m = self.materialized(scope)?.clone();
        std::fs::create_dir_all(out.join("docs")).map_err(io_err)?;
        std::fs::create_dir_all(out.join("blobs")).map_err(io_err)?;
        let mut summary = ExportSummary { docs: 0, blocks: 0, blobs: 0 };
        let mut blobs = BTreeSet::new();
        for doc in m.children(None) {
            let mut list = vec![];
            let mut stack = vec![doc];
            while let Some(b) = stack.pop() {
                list.push(to_yaml_struct(b));
                if let Some(h) = b.field_str("blob").and_then(|s| s.parse::<Hash>().ok()) {
                    blobs.insert(h);
                }
                let mut kids = m.children(Some(&b.id));
                kids.reverse();
                stack.extend(kids);
            }
            summary.blocks += list.len();
            summary.docs += 1;
            let text = serde_yaml_ng::to_string(&list).expect("doc yaml");
            std::fs::write(out.join("docs").join(format!("{}.yaml", doc.id)), text).map_err(io_err)?;
        }
        for h in &blobs {
            std::fs::write(out.join("blobs").join(h.to_string()), self.store.get_blob(h)?).map_err(io_err)?;
            summary.blobs += 1;
        }
        let scope_name = match scope {
            Scope::Canon => "canon".to_string(),
            Scope::View(v) => v.clone(),
        };
        let manifest = Manifest { format: 1, pack: self.pack.name.clone(), scope: scope_name };
        std::fs::write(out.join("rdy-export.yaml"), serde_yaml_ng::to_string(&manifest).expect("manifest")).map_err(io_err)?;
        Ok(summary)
    }

    pub fn import(&mut self, s: &Session, view: &str, dir: &Path) -> Result<WriteResult, RdyError> {
        if !s.role.can_land() {
            return Err(RdyError::new(E_PERMISSION, "import can carry accepted decisions, so it needs an approver", "run the import as an approver session"));
        }
        if view == "canon" {
            return Err(RdyError::new(E_RESERVED_VIEW, "canon cannot be written directly", "import into a view, then propose and land it"));
        }
        let blob_dir = dir.join("blobs");
        if blob_dir.exists() {
            for entry in std::fs::read_dir(&blob_dir).map_err(io_err)? {
                let path = entry.map_err(io_err)?.path();
                let bytes = std::fs::read(&path).map_err(io_err)?;
                let got = self.store.put_blob(&bytes)?;
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                if got.to_string() != name {
                    return Err(RdyError::new(E_BAD_INPUT, format!("blob {name} hashes to {got}"), "the export is corrupt; export it again"));
                }
            }
        }
        let mut files: Vec<_> = std::fs::read_dir(dir.join("docs")).map_err(io_err)?.filter_map(Result::ok).map(|e| e.path()).collect();
        files.sort();
        let mut blocks: Vec<Block> = vec![];
        for f in files {
            let text = std::fs::read_to_string(&f).map_err(io_err)?;
            let list: Vec<BlockYaml> = serde_yaml_ng::from_str(&text).map_err(|e| RdyError::new(E_BAD_INPUT, format!("{}: {e}", f.display()), "fix or re-export the file"))?;
            for y in list {
                blocks.push(from_yaml_struct(y)?);
            }
        }
        let scope = Scope::View(view.to_string());
        let existing = self.materialized(&scope)?.clone();
        let incoming: BTreeMap<BlockId, &Block> = blocks.iter().map(|b| (b.id.clone(), b)).collect();
        let mut issues = vec![];
        for b in &blocks {
            if existing.get(&b.id).is_some() {
                return Err(RdyError::new(E_ALREADY_EXISTS, format!("block {} already exists in view {view}", b.id), "import into a fresh project or view"));
            }
            let exists = |id: &BlockId| incoming.contains_key(id) || existing.get(id).is_some();
            for t in b.ref_tokens() {
                match BlockId::parse(&t) {
                    Ok(id) if exists(&id) => {}
                    _ => return Err(RdyError::new(E_UNKNOWN_REF, format!("{} references {{{{{t}}}}}, which is not in the import", b.id), "export the whole project, or remove the reference")),
                }
            }
            for l in b.links.values().flatten() {
                if !exists(&l.target) {
                    return Err(RdyError::new(E_UNKNOWN_REF, format!("{} links to {}, which is not in the import", b.id, l.target), "export the whole project"));
                }
            }
            let parent = b.parent.as_ref().and_then(|p| incoming.get(p).copied().or_else(|| existing.get(p)));
            issues.extend(validate_block(&self.pack, b, parent));
        }
        if has_errors(&issues) {
            let mut e = RdyError::new(E_VALIDATION, "the import does not validate against this project's pack", "fix the listed blocks or use a compatible pack");
            e.issues = issues;
            return Err(e);
        }
        let ops: Vec<Op> = blocks.into_iter().map(|block| Op::Create { block }).collect();
        let change = Change { view: view.into(), ops: ops.clone(), deps: vec![], provenance: s.provenance(), justification: None, message: format!("import from {}", dir.display()) };
        let id = self.store.commit_to_view(view, change)?;
        self.update_cache(view, &ops);
        let created = ops.iter().enumerate().map(|(i, op)| (format!("op{i}"), op.block_id().to_string())).collect();
        Ok(WriteResult { change: id.to_string(), created, warnings: vec![] })
    }

    pub fn edit_block(&mut self, s: &Session, view: &str, edited: Block) -> Result<WriteResult, RdyError> {
        let scope = Scope::View(view.to_string());
        let cur = self.block(&scope, &edited.id)?;
        if cur.block_type != edited.block_type || cur.parent != edited.parent || cur.order != edited.order {
            return Err(RdyError::new(E_BAD_INPUT, "type, parent and order cannot be changed by edit", "use a move op to change parent or order"));
        }
        let fields = edited.fields.iter().filter(|(k, v)| cur.fields.get(*k) != Some(*v)).map(|(k, v)| (k.clone(), field_json(v))).collect();
        let unset = cur.fields.keys().filter(|k| !edited.fields.contains_key(*k)).cloned().collect();
        let links = (cur.links != edited.links).then(|| {
            edited.links.iter().map(|(k, v)| (k.clone(), v.iter().map(|l| LinkInput { target: l.target.to_string(), part: l.part.clone() }).collect())).collect()
        });
        let op = OpInput::Update { id: edited.id.to_string(), base: Some(cur.version_hash().to_string()), fields, unset, body: (cur.body != edited.body).then(|| edited.body.clone()), links };
        self.write(s, WriteRequest { view: view.into(), ops: vec![op], justification: None, message: format!("edit {}", edited.id) })
    }
}
