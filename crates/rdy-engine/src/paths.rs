//! Paths: readable names for folders and docs, usable wherever an id is (addendum B §2.4).
use crate::error::*;
use crate::{Materialized, Rdy, Scope};
use crate::{Session, WriteRequest, WriteResult};
use rdy_model::{Block, BlockId, tree};
use serde::Serialize;
use serde_json::json;

pub(crate) fn resolve_in(m: &Materialized, path: &str) -> Result<BlockId, RdyError> {
    m.resolve_path(path).map_err(|miss| {
        let at = miss
            .reached
            .clone()
            .unwrap_or_else(|| "the top level".into());
        if miss.ambiguous.len() > 1 {
            return RdyError::new(
                E_UNKNOWN_BLOCK,
                format!(
                    "{path:?} matches more than one name under {at} ignoring case: {}",
                    miss.ambiguous.join(", ")
                ),
                "write the name with its exact case",
            );
        }
        let fix = if miss.children.is_empty() {
            format!("{at} is empty; list the tree with rdy ls")
        } else {
            format!("names under {at}: {}", miss.children.join(", "))
        };
        RdyError::new(
            E_UNKNOWN_BLOCK,
            format!("no folder or doc at {path:?}"),
            fix,
        )
    })
}

impl Rdy {
    pub fn resolve_path(&mut self, scope: &Scope, path: &str) -> Result<BlockId, RdyError> {
        resolve_in(self.materialized(scope)?, path)
    }

    /// Text matching the id grammar is an id (not checked to exist); anything else is a path.
    pub fn resolve_id(&mut self, scope: &Scope, text: &str) -> Result<BlockId, RdyError> {
        match BlockId::parse(text) {
            Ok(id) => Ok(id),
            Err(_) => self.resolve_path(scope, text),
        }
    }

    pub fn path_of(&mut self, scope: &Scope, id: &BlockId) -> Result<Option<String>, RdyError> {
        let m = self.materialized(scope)?;
        if m.get(id).is_none() {
            return Err(RdyError::new(
                E_UNKNOWN_BLOCK,
                format!("no block {id}"),
                "check the id with rdy_read",
            ));
        }
        Ok(m.path_of(id))
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TreeNode {
    pub path: String,
    pub name: String,
    pub id: String,
    #[serde(rename = "type")]
    pub block_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TreeNode>,
    /// Folders and docs below this node that are left out.
    #[serde(skip_serializing_if = "is_zero")]
    pub collapsed: usize,
}

fn count_below(m: &Materialized, id: &BlockId) -> usize {
    m.children(Some(id))
        .into_iter()
        .filter(|b| tree::is_tree_type(&b.block_type))
        .map(|b| 1 + count_below(m, &b.id))
        .sum()
}

fn node(m: &Materialized, b: &Block, path: String, name: String, depth: Option<u32>) -> TreeNode {
    let (children, collapsed) = match (b.block_type == tree::FOLDER, depth) {
        (false, _) => (vec![], 0),
        (true, Some(0)) => (vec![], count_below(m, &b.id)),
        (true, d) => (nodes_under(m, Some(&b.id), &path, d.map(|d| d - 1)), 0),
    };
    TreeNode {
        name,
        id: b.id.to_string(),
        block_type: b.block_type.clone(),
        title: b.field_str("title").map(String::from),
        doc_type: b.field_str("doc_type").map(String::from),
        lifecycle: b.field_str("lifecycle").map(String::from),
        children,
        collapsed,
        path,
    }
}

fn nodes_under(
    m: &Materialized,
    parent: Option<&BlockId>,
    prefix: &str,
    depth: Option<u32>,
) -> Vec<TreeNode> {
    let names = m.names_under(parent);
    m.children(parent)
        .into_iter()
        .filter_map(|b| {
            let name = names.get(&b.id)?.clone();
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            Some(node(m, b, path, name, depth))
        })
        .collect()
}

impl Rdy {
    pub fn tree(
        &mut self,
        scope: &Scope,
        path: Option<&str>,
        depth: Option<u32>,
    ) -> Result<Vec<TreeNode>, RdyError> {
        let m = self.materialized(scope)?;
        let Some(p) = path else {
            return Ok(nodes_under(m, None, "", depth));
        };
        let id = resolve_in(m, p)?;
        let b = m.get(&id).expect("resolved");
        let full = m.path_of(&id).unwrap_or_default();
        let name = m.name_of(&id).unwrap_or_default();
        Ok(vec![node(m, b, full, name, depth)])
    }

    pub fn new_folder(
        &mut self,
        s: &Session,
        view: &str,
        path: &str,
    ) -> Result<WriteResult, RdyError> {
        self.new_at(s, view, path, None)
    }

    pub fn new_doc(
        &mut self,
        s: &Session,
        view: &str,
        path: &str,
        doc_type: Option<&str>,
        title: Option<&str>,
    ) -> Result<WriteResult, RdyError> {
        let dt = match doc_type {
            Some(t) => t.to_string(),
            None => self.pack.default_doc_type.clone().ok_or_else(|| {
                RdyError::new(
                    E_BAD_INPUT,
                    format!("pack {} has no default_doc_type", self.pack.name),
                    format!(
                        "pass --type, one of: {}",
                        self.pack
                            .doc_types
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            })?,
        };
        self.new_at(s, view, path, Some((dt, title.map(String::from))))
    }

    fn new_at(
        &mut self,
        s: &Session,
        view: &str,
        path: &str,
        doc: Option<(String, Option<String>)>,
    ) -> Result<WriteResult, RdyError> {
        let segs: Vec<&str> = path.split('/').filter(|x| !x.is_empty()).collect();
        let Some((last, dirs)) = segs.split_last() else {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "the path is empty",
                "pass a path such as wallets/transactions",
            ));
        };
        let m = self.materialized(&Scope::View(view.to_string()))?;
        let mut parent: Option<BlockId> = None;
        let mut parent_ref: Option<String> = None;
        let mut ops = vec![];
        for (i, seg) in dirs.iter().enumerate() {
            if ops.is_empty()
                && let Ok(id) = m.child_named(parent.as_ref(), seg)
            {
                let b = m.get(&id).expect("named child exists");
                if b.block_type != tree::FOLDER {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        format!(
                            "{} is a {}, not a folder",
                            m.path_of(&id).unwrap_or_default(),
                            b.block_type
                        ),
                        "put new folders and docs inside a folder",
                    ));
                }
                parent_ref = Some(id.to_string());
                parent = Some(id);
                continue;
            }
            let ph = format!("$folder{i}");
            ops.push(json!({"op": "create", "type": tree::FOLDER, "id": ph, "parent": parent_ref, "fields": {"name": seg}}));
            parent_ref = Some(ph);
        }
        if ops.is_empty() && m.child_named(parent.as_ref(), last).is_ok() {
            return Err(RdyError::new(
                E_PATH_CLASH,
                format!("{path} already exists"),
                "choose another name, or edit the existing one",
            ));
        }
        let kind = match &doc {
            None => {
                ops.push(json!({"op": "create", "type": tree::FOLDER, "parent": parent_ref, "fields": {"name": last}}));
                "folder"
            }
            Some((dt, title)) => {
                ops.push(json!({"op": "create", "type": tree::DOC, "id": "$doc", "parent": parent_ref,
                    "fields": {"name": last, "title": title.clone().unwrap_or_else(|| last.to_string()), "doc_type": dt}}));
                if self.pack.block_types.contains_key("section") {
                    for (k, sec) in self
                        .pack
                        .doc_types
                        .get(dt)
                        .map(|d| d.skeleton.clone())
                        .unwrap_or_default()
                        .iter()
                        .enumerate()
                    {
                        ops.push(json!({"op": "create", "type": "section", "id": format!("$s{k}"), "parent": "$doc", "fields": {"title": sec.title}}));
                    }
                }
                "doc"
            }
        };
        let ops = serde_json::from_value(serde_json::Value::Array(ops))
            .expect("ops built above are valid");
        self.write(
            s,
            WriteRequest {
                view: view.into(),
                ops,
                justification: None,
                message: format!("new {kind} {path}"),
            },
        )
    }
}
