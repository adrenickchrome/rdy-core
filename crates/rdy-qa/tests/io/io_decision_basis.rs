//! rf5 (D-2WR0G, D-JHV8P, D-BT8RE, D-JCVPD): an import keeps each block's original author and
//! time, records the importer on every change, and checks each imported decision's basis and
//! times before it stores a blob or a change.
use rdy_engine::decision_fields::{ATTESTED_BY_FIX, ATTESTED_BY_MESSAGE, TIME_FIX};
use rdy_engine::*;
use rdy_model::*;
use serde_json::json;
use std::path::Path;

fn sess(actor: &str, run: Option<&str>, role: Role) -> Session {
    Session {
        actor: actor.into(),
        run: run.map(str::to_string),
        role,
        kind: SessionKind::Human,
    }
}

fn put(r: &mut Rdy, s: &Session, ops: serde_json::Value) -> WriteResult {
    r.write(
        s,
        WriteRequest {
            view: "work".into(),
            ops: parse_ops(serde_json::from_value(ops).unwrap()).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap()
}

/// The source project, in view `work`: agent:a writes doc `register`, its section, a figure and
/// a delegated decision; agent:b writes a source and a finding citing it. Returns the project,
/// the ids by name and the figure's blob.
struct Source {
    r: Rdy,
    doc: String,
    section: String,
    figure: String,
    decision: String,
    source: String,
    finding: String,
    blob: Hash,
}

const EFFECTIVE: &str = "2026-10-21T15:10:58.5+00:00";

fn source_project(dir: &Path) -> Source {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let a = sess("agent:a", Some("r1"), Role::Writer);
    let b = sess("agent:b", Some("r2"), Role::Writer);
    let blob = r.store_mut().put_blob(b"figure bytes").unwrap();
    let w = put(
        &mut r,
        &a,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "register", "title": "Register", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rulings"}},
            {"op": "create", "type": "figure", "id": "$g", "parent": "$s", "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": "bytes"}}
        ]),
    );
    let (doc, section, figure) = (
        w.created["$d"].clone(),
        w.created["$s"].clone(),
        w.created["$g"].clone(),
    );
    let w = put(
        &mut r,
        &b,
        json!([
            {"op": "create", "type": "source", "id": "$src", "parent": section, "fields": {"title": "Release 2", "kind": "release", "locator": "v2"}},
            {"op": "create", "type": "finding", "id": "$f", "parent": section, "fields": {"statement": "The release changed retention.", "evidence": [{"quote": "Retention is now seven days.", "session_ref": "s4"}, {"source": "{{$src}}"}]}}
        ]),
    );
    let (source, finding) = (w.created["$src"].clone(), w.created["$f"].clone());
    let w = r
        .decide(
            &a,
            "work",
            DecisionInput {
                parent: section.clone(),
                statement: "Retention follows the release".into(),
                authority_basis: Some("delegated".into()),
                attested_by: Some("Nick, under the retention mandate".into()),
                review_by: Some("2027-01-01".into()),
                effective_from: Some(EFFECTIVE.into()),
                depends_on: vec![finding.clone()],
                ..Default::default()
            },
        )
        .unwrap();
    let decision = w.created["$decision"].clone();
    Source {
        r,
        doc,
        section,
        figure,
        decision,
        source,
        finding,
        blob,
    }
}

/// Exports view `work` of `src` to `out` with interleaved author times: agent:a's doc and
/// section at 100, agent:b's source at 150 and finding at 200, agent:a's figure and decision at
/// 300.
fn export(src: &Source, out: &Path) {
    src.r
        .read()
        .export(&Scope::View("work".into()), out)
        .unwrap();
    let row = |id: &str, actor: &str, run: &str, at: i64| {
        format!("- id: {id}\n  actor: {actor}\n  run: {run}\n  role: writer\n  at_ms: {at}\n")
    };
    let text = [
        row(&src.doc, "agent:a", "r1", 100),
        row(&src.section, "agent:a", "r1", 100),
        row(&src.source, "agent:b", "r2", 150),
        row(&src.finding, "agent:b", "r2", 200),
        row(&src.figure, "agent:a", "r1", 300),
        row(&src.decision, "agent:a", "r1", 300),
    ]
    .concat();
    std::fs::write(out.join("authors.yaml"), text).unwrap();
}

/// Replaces `from` with `to` in every file under `dir`; panics unless one changed.
fn rewrite(dir: &Path, from: &str, to: &str) {
    fn walk(d: &Path, from: &str, to: &str, hits: &mut usize) {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, from, to, hits);
            } else if let Ok(text) = std::fs::read_to_string(&p)
                && text.contains(from)
            {
                std::fs::write(&p, text.replace(from, to)).unwrap();
                *hits += 1;
            }
        }
    }
    let mut hits = 0;
    walk(&dir.join("docs"), from, to, &mut hits);
    assert!(hits > 0, "{from} is in no exported file");
}

fn block(r: &Rdy, view: &str, id: &str) -> Block {
    r.read()
        .block(&Scope::View(view.into()), &BlockId::parse(id).unwrap())
        .unwrap()
}

#[test]
fn rf5_import_keeps_original_actor_and_records_importer() {
    let a_dir = tempfile::tempdir().unwrap();
    let src = source_project(a_dir.path());
    let out_dir = tempfile::tempdir().unwrap();
    let out = out_dir.path().join("x");
    export(&src, &out);
    let imp = sess("human:imp", None, Role::Approver);

    // bad decision data: refused before a blob, a change or the view is stored
    for (from, to, want) in [
        (
            "2027-01-01",
            "soon",
            (
                "E_FIELD_KIND",
                "field \"review_by\" must be an RFC 3339 UTC time or a date".to_string(),
                TIME_FIX.to_string(),
            ),
        ),
        (
            EFFECTIVE,
            "2026-10-21T15:10:58+01:00",
            (
                "E_FIELD_KIND",
                "field \"effective_from\" must be an RFC 3339 UTC time or a date".to_string(),
                TIME_FIX.to_string(),
            ),
        ),
        (
            "Nick, under the retention mandate",
            "\"   \"",
            (
                "E_FIELD_REQUIRED",
                ATTESTED_BY_MESSAGE.to_string(),
                ATTESTED_BY_FIX.to_string(),
            ),
        ),
    ] {
        let bad = out_dir
            .path()
            .join(format!("bad-{to}").replace([' ', ':', '+', '"'], "_"));
        copy_dir(&out, &bad);
        rewrite(&bad, from, to);
        let b_dir = tempfile::tempdir().unwrap();
        let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
        let seq = b.read().seq();
        let e = b.import(&imp, "import", &bad).unwrap_err();
        assert_eq!(e.code, E_VALIDATION, "{e}");
        let issues: Vec<_> = e
            .issues
            .iter()
            .map(|i| (i.code, i.message.clone(), i.fix.clone()))
            .collect();
        assert_eq!(issues, vec![want.clone()], "{from}");
        assert_eq!(
            e.issues[0].block.as_ref().map(ToString::to_string),
            Some(src.decision.clone())
        );
        assert_eq!(b.read().seq(), seq, "{from}: no change");
        assert!(!b.store().has_blob(&src.blob).unwrap(), "{from}: no blob");
        assert!(b.read().view_head("import").is_none(), "{from}: no view");
    }

    // the good export: one change per author, the author's run and time, the importer on each
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    let res = b.import(&imp, "import", &out).unwrap();
    assert_eq!(res.changes.len(), 2, "{res:?}");
    {
        let st = b.read();
        let ch = |h: &str| st.reader().change(&h.parse::<Hash>().unwrap()).unwrap();
        let (one, two) = (ch(&res.changes[0]), ch(&res.changes[1]));
        assert_eq!(
            (one.provenance.actor.as_str(), one.provenance.run.as_deref()),
            ("agent:b", Some("r2"))
        );
        assert_eq!(one.provenance.at_ms, 200);
        assert_eq!(
            (two.provenance.actor.as_str(), two.provenance.run.as_deref()),
            ("agent:a", Some("r1"))
        );
        assert_eq!(two.provenance.at_ms, 300);
        let ids = |c: &Change| {
            let mut v: Vec<String> = c.ops.iter().map(|o| o.block_id().to_string()).collect();
            v.sort();
            v
        };
        let mut want_b = vec![src.source.clone(), src.finding.clone()];
        want_b.sort();
        assert_eq!(ids(&one), want_b);
        let mut want_a = vec![
            src.doc.clone(),
            src.section.clone(),
            src.figure.clone(),
            src.decision.clone(),
        ];
        want_a.sort();
        assert_eq!(ids(&two), want_a);
        for c in [&one, &two] {
            assert_eq!(
                c.links.imported_by,
                Some(ActorRun {
                    actor: "human:imp".into(),
                    run: None
                })
            );
        }
    }
    assert!(b.store().has_blob(&src.blob).unwrap());
    // every field kept its bytes: the decision's basis and times, the finding's evidence
    for id in [&src.decision, &src.finding, &src.source] {
        assert_eq!(
            block(&b, "import", id).fields,
            block(&src.r, "work", id).fields,
            "{id}"
        );
    }
    let d = block(&b, "import", &src.decision);
    assert_eq!(d.field_str("effective_from"), Some(EFFECTIVE));
    assert_eq!(d.field_str("authority_basis"), Some("delegated"));
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let t = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_dir(&p, &t);
        } else {
            std::fs::copy(&p, &t).unwrap();
        }
    }
}
