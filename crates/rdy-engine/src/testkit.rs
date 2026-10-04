//! Synthetic projects for benchmarks and performance tests.
use crate::{Rdy, Session, WriteRequest};
use rdy_model::{BlockId, Role};
use serde_json::json;
use std::path::Path;

pub fn generate(project: &Path, blocks: usize) -> Vec<BlockId> {
    let mut r = Rdy::init(project, "rdy-standard").expect("init");
    let s = Session { actor: "agent:bench".into(), run: None, role: Role::Writer };
    let mut ids = vec![];
    let mut doc_no = 0;
    while ids.len() < blocks {
        let mut ops: Vec<serde_json::Value> = vec![json!({"op": "create", "type": "doc", "id": "$d", "fields": {"title": format!("Doc {doc_no}"), "doc_type": "constraints"}})];
        for sec in 0..10 {
            ops.push(json!({"op": "create", "type": "section", "id": format!("$s{sec}"), "parent": "$d", "fields": {"title": if sec == 0 { "Constraints".to_string() } else { format!("Part {sec}") }}}));
        }
        let res = r.write(&s, WriteRequest { view: "work".into(), ops: serde_json::from_value(json!(ops)).unwrap(), justification: None, message: String::new() }).expect("doc");
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
                    let r2 = r.write(&s, WriteRequest { view: "work".into(), ops: serde_json::from_value(json!(batch)).unwrap(), justification: None, message: String::new() }).expect("claims");
                    let mut created: Vec<(usize, String)> = r2.created.iter().map(|(k, v)| (k.trim_start_matches("$c").parse().unwrap(), v.clone())).collect();
                    created.sort();
                    prev = created.last().map(|(_, v)| v.clone());
                    ids.extend(created.into_iter().map(|(_, v)| BlockId::parse(&v).unwrap()));
                    batch.clear();
                }
            }
            if !batch.is_empty() {
                let r2 = r.write(&s, WriteRequest { view: "work".into(), ops: serde_json::from_value(json!(batch)).unwrap(), justification: None, message: String::new() }).expect("claims");
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
