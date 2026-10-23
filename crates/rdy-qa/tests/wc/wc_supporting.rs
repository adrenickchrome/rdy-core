#![cfg(unix)]
// Unix only: it runs the working-copy CLI against a served project. FW-XP1 Phase 4 ports it.
//! rf5 (D-Z8P09, n-QJ36D): findings and sources live only in the store. A checkout and a refresh
//! write neither, nor a link to one; an entry of either type put in a file by hand is refused
//! with the supporting diagnostic; a normal commit, deletions included, keeps both.
use rdy_engine::E_WC_INVALID;
use rdy_qa::daemon::Served;
use rdy_qa::wc::*;
use serde_json::{Value, json};

struct Planted {
    finding: String,
    source: String,
}

/// The seeded project with a source and a finding under the intro's section, the finding quoting
/// the claim and citing the source, and the claim depending on the source.
fn planted() -> (Served, Seed, Planted) {
    let (s, seed) = seeded();
    let w = store_write(
        &s,
        "work",
        json!([
            {"op": "create", "type": "source", "id": "$src", "parent": seed.section,
             "fields": {"title": "Release 2", "kind": "release", "locator": "v2"}},
            {"op": "create", "type": "finding", "id": "$f", "parent": seed.section,
             "fields": {"statement": "The release changed retention.",
                        "evidence": [{"quote": format!("{{{{{}}}}} holds.", seed.claim)}, {"source": "{{$src}}"}]}},
            {"op": "update", "id": seed.claim, "links": {"depends_on": [{"target": "$src"}]}}
        ]),
    );
    let p = Planted {
        finding: w.created["$f"].clone(),
        source: w.created["$src"].clone(),
    };
    (s, seed, p)
}

/// The project's files hold neither block, nor a link to either.
fn files_clean(s: &Served, p: &Planted) {
    for rel in ["guides/intro.yaml", "notes.yaml"] {
        let text = read(s, rel);
        for id in [&p.finding, &p.source] {
            assert!(!text.contains(id.as_str()), "{rel} names {id}:\n{text}");
        }
        assert!(
            !text.contains("finding") && !text.contains("depends_on"),
            "{rel}:\n{text}"
        );
    }
}

fn kept(s: &Served, p: &Planted, seed: &Seed) {
    let f = store_block(s, "work", &p.finding);
    assert_eq!(f.block_type, "finding");
    let src = store_block(s, "work", &p.source);
    assert_eq!(src.block_type, "source");
    let claim = store_block(s, "work", &seed.claim);
    assert_eq!(
        claim.links["depends_on"]
            .iter()
            .map(|l| l.target.to_string())
            .collect::<Vec<_>>(),
        vec![p.source.clone()],
        "the store-only link is kept"
    );
}

#[test]
fn rf5_working_copy_never_carries_or_deletes_supporting() {
    let (s, seed, p) = planted();
    ok(&s, &["checkout"]);
    files_clean(&s, &p);

    // an entry of either type put in a file by hand is the supporting diagnostic, nested in the
    // commit's E_WC_INVALID; nothing is recorded
    let original = read(&s, "guides/intro.yaml");
    for (ty, id, fields) in [
        (
            "finding",
            "F-QQQQQ",
            "statement: Hand made.\n    evidence:\n    - quote: By hand.\n",
        ),
        (
            "source",
            "S-QQQQQ",
            "title: Release 3\n    kind: release\n    locator: v3\n",
        ),
    ] {
        let text = format!(
            "{original}- id: {id}\n  type: {ty}\n  parent: {}\n  fields:\n    {fields}",
            seed.section
        );
        put(&s, "guides/intro.yaml", &text);
        let want = |d: &Value| {
            d["code"] == "E_WC_SUPPORTING"
                && d["file"] == "guides/intro.yaml"
                && d["message"] == format!("{id} is a {ty}, which lives only in the store")
                && d["fix"]
                    == format!(
                        "record it with rdy_write or rdy new {ty}, then remove the entry from the file"
                    )
        };
        let o = s.rdy(&["--json", "check"]);
        assert!(!o.status.success(), "{ty}: check passed");
        let diags: Value = serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|_| serde_json::from_slice(&o.stderr).unwrap());
        assert!(diags.as_array().unwrap().iter().any(want), "{ty}: {diags}");
        ok(&s, &["add", "guides/intro.yaml"]);
        let (good, v) = json(&s, &["commit", "-m", "by hand"]);
        assert!(!good && v["code"] == E_WC_INVALID, "{ty}: {v}");
        assert!(
            v["diagnostics"].as_array().unwrap().iter().any(want),
            "{ty}: {v}"
        );
        put(&s, "guides/intro.yaml", &original);
        ok(&s, &["add", "guides/intro.yaml"]);
    }
    kept(&s, &p, &seed);

    // a normal commit, with an edit and a deletion, keeps both and the claim's link
    set_body(&s, "guides/intro.yaml", &seed.claim, "Claims still hold.");
    edit(&s, "notes.yaml", |l| {
        l.retain(|e| e["id"].as_str() != Some(seed.note.as_str()))
    });
    ok(&s, &["add", "guides/intro.yaml", "notes.yaml"]);
    ok(&s, &["commit", "-m", "edit and delete"]);
    kept(&s, &p, &seed);
    assert_eq!(
        store_block(&s, "work", &seed.claim).body,
        "Claims still hold."
    );
    files_clean(&s, &p);

    // a change to both made elsewhere comes in by a refresh, which writes neither
    store_write(
        &s,
        "work",
        json!([
            {"op": "update", "id": p.finding, "fields": {"statement": "The release cut retention."}},
            {"op": "update", "id": p.source, "fields": {"version": "2.0.1"}}
        ]),
    );
    let (good, v) = json(&s, &["update"]);
    assert!(good, "{v}");
    files_clean(&s, &p);
    kept(&s, &p, &seed);
}
