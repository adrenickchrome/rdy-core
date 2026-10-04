//! Synthetic projects for benchmarks and performance tests.
use crate::{Rdy, Session, WriteRequest};
use rdy_model::{BlockId, Role};
use serde_json::json;
use std::path::Path;

pub fn generate(project: &Path, blocks: usize) -> Vec<BlockId> {
    let mut r = Rdy::init(project, "rdy-standard").expect("init");
    let s = Session {
        actor: "agent:bench".into(),
        run: None,
        role: Role::Writer,
    };
    let mut ids = vec![];
    let mut doc_no = 0;
    while ids.len() < blocks {
        let mut ops: Vec<serde_json::Value> = vec![
            json!({"op": "create", "type": "doc", "id": "$d", "fields": {"title": format!("Doc {doc_no}"), "doc_type": "constraints"}}),
        ];
        for sec in 0..10 {
            ops.push(json!({"op": "create", "type": "section", "id": format!("$s{sec}"), "parent": "$d", "fields": {"title": if sec == 0 { "Constraints".to_string() } else { format!("Part {sec}") }}}));
        }
        let res = r
            .write(
                &s,
                WriteRequest {
                    view: "work".into(),
                    ops: serde_json::from_value(json!(ops)).unwrap(),
                    justification: None,
                    message: String::new(),
                },
            )
            .expect("doc");
        for sec in 0..10 {
            let parent = res.created[&format!("$s{sec}")].clone();
            let mut prev: Option<String> = None;
            let mut batch: Vec<serde_json::Value> = vec![];
            for i in 0..100 {
                if ids.len() + batch.len() >= blocks {
                    break;
                }
                let body = match (&prev, i % 10) {
                    (Some(p), 0) => format!("Claim {i} builds on {{{{{p}}}}}."),
                    _ => format!("Claim {i} in doc {doc_no} section {sec}."),
                };
                batch.push(json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": parent, "body": body}));
                if i % 10 == 9 {
                    let r2 = r
                        .write(
                            &s,
                            WriteRequest {
                                view: "work".into(),
                                ops: serde_json::from_value(json!(batch)).unwrap(),
                                justification: None,
                                message: String::new(),
                            },
                        )
                        .expect("claims");
                    let mut created: Vec<(usize, String)> = r2
                        .created
                        .iter()
                        .map(|(k, v)| (k.trim_start_matches("$c").parse().unwrap(), v.clone()))
                        .collect();
                    created.sort();
                    prev = created.last().map(|(_, v)| v.clone());
                    ids.extend(
                        created
                            .into_iter()
                            .map(|(_, v)| BlockId::parse(&v).unwrap()),
                    );
                    batch.clear();
                }
            }
            if !batch.is_empty() {
                let r2 = r
                    .write(
                        &s,
                        WriteRequest {
                            view: "work".into(),
                            ops: serde_json::from_value(json!(batch)).unwrap(),
                            justification: None,
                            message: String::new(),
                        },
                    )
                    .expect("claims");
                ids.extend(r2.created.values().map(|v| BlockId::parse(v).unwrap()));
            }
            if ids.len() >= blocks {
                break;
            }
        }
        doc_no += 1;
    }
    ids
}

/// A tree-shaped synthetic project: `folders` folders (10 top level, the rest under them), each
/// with `docs_per_folder` docs of one section holding `claims_per_doc` claims. Returns doc paths.
pub fn generate_tree(
    project: &Path,
    folders: usize,
    docs_per_folder: usize,
    claims_per_doc: usize,
) -> Vec<String> {
    let mut r = Rdy::init(project, "rdy-standard").expect("init");
    let s = Session {
        actor: "agent:bench".into(),
        run: None,
        role: Role::Writer,
    };
    let w = |r: &mut Rdy, ops: Vec<serde_json::Value>| {
        r.write(
            &s,
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!(ops)).unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .expect("write")
    };
    let top = folders.min(10);
    let mut folder_paths: Vec<(String, String)> = vec![];
    let res = w(
        &mut r,
        (0..top)
            .map(|i| json!({"op": "create", "type": "folder", "id": format!("$f{i}"), "fields": {"name": format!("area-{i}")}}))
            .collect(),
    );
    for i in 0..top {
        folder_paths.push((res.created[&format!("$f{i}")].clone(), format!("area-{i}")));
    }
    for i in top..folders {
        let (pid, ppath) = folder_paths[i % top].clone();
        let res = w(
            &mut r,
            vec![
                json!({"op": "create", "type": "folder", "parent": pid, "fields": {"name": format!("part-{i}")}}),
            ],
        );
        folder_paths.push((res.created["op0"].clone(), format!("{ppath}/part-{i}")));
    }
    let mut docs = vec![];
    for (fid, fpath) in &folder_paths {
        for d in 0..docs_per_folder {
            let mut ops = vec![
                json!({"op": "create", "type": "doc", "id": "$d", "parent": fid, "fields": {"name": format!("doc-{d}"), "title": format!("Doc {d}"), "doc_type": "constraints"}}),
                json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Constraints"}}),
            ];
            for c in 0..claims_per_doc {
                ops.push(json!({"op": "create", "type": "claim", "parent": "$s", "body": format!("Claim {c} of doc {d}.")}));
            }
            w(&mut r, ops);
            docs.push(format!("{fpath}/doc-{d}"));
        }
    }
    docs
}
