use rdy_engine::*;
use rdy_model::*;
use rdy_render::*;
use rdy_store::Store;
use serde_json::json;

fn agent() -> Session {
    Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
    }
}
fn founder() -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
    }
}

#[test]
fn text_html_escapes_then_links() {
    let t = BlockId::parse("t-MEDGR").unwrap();
    let html = text_html("<b>x</b> & {{t-MEDGR}} {{t-N0PE1}}\nnext", &|id| {
        (id == &t).then(|| ("doc/d-AAAAA.html#t-MEDGR".into(), "Ledger".into()))
    });
    assert_eq!(
        html,
        "&lt;b&gt;x&lt;/b&gt; &amp; <a href=\"doc/d-AAAAA.html#t-MEDGR\">Ledger</a> <span class=\"ref missing\">t-N0PE1</span><br>next"
    );
}

#[test]
fn renders_docs_decisions_diagrams_and_assets() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let blob = r.store_mut().put_blob(b"\x89PNG fake").unwrap().to_string();
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Identity", "doc_type": "components"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Components"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "The <record>."},
        {"op": "create", "type": "claim", "parent": "$s", "body": "Balances live on the {{$t}}. <script>alert(1)</script>"},
        {"op": "create", "type": "diagram", "parent": "$s", "fields": {"lang": "mermaid", "alt": "flow"}, "body": "graph TD\n  A --> B"},
        {"op": "create", "type": "figure", "parent": "$s", "fields": {"blob": blob, "media_type": "image/png", "alt": "logo"}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let d = r
        .decide(
            &agent(),
            "work",
            DecisionInput {
                parent: res.created["$s"].clone(),
                statement: "Ledger is append-only".into(),
                options: vec!["append-only".into(), "mutable".into()],
                ..Default::default()
            },
        )
        .unwrap();
    let p = r.propose(&agent(), "work", "seed").unwrap();
    r.land(&founder(), &p.id).unwrap();

    let out = tempfile::tempdir().unwrap();
    let summary = render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    assert_eq!(summary.pages, 3); // index, one doc, one decision
    assert_eq!(summary.assets, 1);

    let t_id = &res.created["$t"];
    let did = &d.created["$decision"];
    let doc = std::fs::read_to_string(out.path().join("identity.html")).unwrap();
    assert!(doc.contains("<title>Identity"));
    assert!(
        doc.contains(&format!("href=\"#{t_id}\">Ledger</a>")),
        "same-doc reference links to the anchor"
    );
    assert!(doc.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!doc.contains("<script>alert(1)"));
    assert!(doc.contains("<pre class=\"mermaid\">graph TD\n  A --&gt; B</pre>"));
    assert!(doc.contains("mermaid"));
    assert!(doc.contains(&format!("src=\"_assets/{blob}\"")) && doc.contains("alt=\"logo\""));
    assert!(doc.contains(&format!("href=\"_decision/{did}.html\"")));

    let dec = std::fs::read_to_string(out.path().join(format!("_decision/{did}.html"))).unwrap();
    assert!(dec.contains("Ledger is append-only"));
    assert!(dec.contains("live"));
    assert!(dec.contains("mutable"), "considered options are listed");

    let index = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(index.contains(&"identity.html".to_string()));
    assert!(
        !index.contains("cdn.jsdelivr.net/npm/mermaid"),
        "no diagram script on pages without diagrams"
    );
    assert!(out.path().join(format!("_assets/{blob}")).exists());
}

/// Commits `blocks` to `view` straight through the store, past the write path's checks: an older
/// project can hold figures whose blob is missing or junk.
fn raw_create(r: &mut Rdy, view: &str, blocks: Vec<Block>) {
    let change = Change {
        view: view.into(),
        ops: blocks
            .into_iter()
            .map(|block| Op::Create { block })
            .collect(),
        deps: vec![],
        provenance: agent().provenance(),
        justification: None,
        message: "raw".into(),
    };
    r.store_mut().commit_to_view(view, change).unwrap();
    r.invalidate();
}

fn raw_figure(id: &str, parent: &str, order: &str, blob: &str, alt: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "figure".into(),
        parent: Some(BlockId::parse(parent).unwrap()),
        order: order.into(),
        fields: [("blob", blob), ("media_type", "image/png"), ("alt", alt)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), FieldValue::Str(v.to_string())))
            .collect(),
        body: String::new(),
        links: Default::default(),
        pins: Default::default(),
    }
}

#[test]
fn dangling_and_junk_blobs_become_placeholders() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let absent = Store::blob_hash(b"never stored").to_string();
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Assets", "doc_type": "components"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "S"}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let s = res.created["$s"].clone();
    raw_create(
        &mut r,
        "work",
        vec![
            raw_figure("fg-AAAAA", &s, "a1", &absent, "gone"),
            raw_figure("fg-BBBBB", &s, "a2", "not-a-hash", "junk"),
        ],
    );
    let p = r.propose(&agent(), "work", "seed").unwrap();
    r.land(&founder(), &p.id).unwrap();
    let out = tempfile::tempdir().unwrap();
    let summary = render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    assert_eq!(summary.assets, 0);
    let doc = std::fs::read_to_string(out.path().join("assets.html")).unwrap();
    assert!(doc.contains("<p class=\"asset missing\">missing asset: gone</p>"));
    assert!(doc.contains("<p class=\"asset missing\">missing asset: junk</p>"));
    assert!(!doc.contains("<img"));
}

#[test]
fn hostile_text_is_escaped_in_attribute_contexts() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let blob = r.store_mut().put_blob(b"img").unwrap().to_string();
    let evil = "x\" onerror=\"alert(1)\" <b>";
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "T", "doc_type": "components"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "S"}},
        {"op": "create", "type": "figure", "parent": "$s", "fields": {"blob": blob, "media_type": "image/png", "alt": evil}},
        {"op": "create", "type": "code", "parent": "$s", "fields": {"lang": evil}, "body": "x"}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let d = r
        .decide(
            &agent(),
            "work",
            DecisionInput {
                parent: res.created["$s"].clone(),
                statement: evil.into(),
                options: vec![evil.into()],
                ..Default::default()
            },
        )
        .unwrap();
    let p = r.propose(&agent(), "work", "seed").unwrap();
    r.land(&founder(), &p.id).unwrap();
    let out = tempfile::tempdir().unwrap();
    render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let doc = std::fs::read_to_string(out.path().join("t.html")).unwrap();
    let dec = std::fs::read_to_string(
        out.path()
            .join(format!("_decision/{}.html", d.created["$decision"])),
    )
    .unwrap();
    let index = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    for page in [&doc, &dec, &index] {
        assert!(!page.contains("onerror=\"alert"), "attribute breakout");
        assert!(!page.contains("<b>"), "raw tag");
    }
    assert!(doc.contains("alt=\"x&quot; onerror=&quot;alert(1)&quot; &lt;b&gt;\""));
    assert!(doc.contains("language-x&quot; onerror=&quot;alert(1)&quot; &lt;b&gt;"));
}

#[test]
fn escaped_braces_render_literally_and_diagram_references_become_labels() {
    assert_eq!(
        text_html(r"Write \{{literal}} or {{t-N0PE1}}", &|_| None),
        "Write {{literal}} or <span class=\"ref missing\">t-N0PE1</span>"
    );
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Flows", "doc_type": "components"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Components"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger & co"}, "body": "Balances."},
        {"op": "create", "type": "diagram", "parent": "$s", "fields": {"lang": "mermaid", "alt": "flow"}, "body": "graph TD\n  A{{Decide?}} --> B[{{$t}}]"},
        {"op": "create", "type": "code", "parent": "$s", "fields": {"lang": "yaml"}, "body": r"x: \{{ .Values.image }}"}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let out = tempfile::tempdir().unwrap();
    render_site(&mut r, &Scope::View("work".into()), out.path()).unwrap();
    let doc = std::fs::read_to_string(out.path().join("flows.html")).unwrap();
    assert!(
        doc.contains(
            "<pre class=\"mermaid\">graph TD\n  A{{Decide?}} --&gt; B[Ledger &amp; co]</pre>"
        ),
        "{doc}"
    );
    assert!(
        doc.contains(r"x: \{{ .Values.image }}"),
        "code is shown verbatim"
    );
}

#[test]
fn link_labels_render_references_as_text_on_one_line() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Ledger", "doc_type": "components"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "S"}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let s = res.created["$s"].clone();
    let d = r
        .decide(
            &agent(),
            "work",
            DecisionInput {
                parent: s.clone(),
                statement: "Ledger is append-only".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let did = d.created["$decision"].clone();
    r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "claim", "parent": s, "body": format!("Corrections are\nnew entries per {{{{{did}}}}}.")}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let p = r.propose(&agent(), "work", "seed").unwrap();
    r.land(&founder(), &p.id).unwrap();
    let out = tempfile::tempdir().unwrap();
    render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let dec = std::fs::read_to_string(out.path().join(format!("_decision/{did}.html"))).unwrap();
    assert!(
        dec.contains(">Corrections are new entries per Ledger is append-only.</a>"),
        "Referenced by label: {dec}"
    );
}

fn small_site(r: &mut Rdy) -> (String, String) {
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Identity", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Decisions"}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let d = r
        .decide(
            &agent(),
            "work",
            DecisionInput {
                parent: res.created["$s"].clone(),
                statement: "Ledger is append-only".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let p = r.propose(&agent(), "work", "seed").unwrap();
    r.land(&founder(), &p.id).unwrap();
    (res.created["$doc"].clone(), d.created["$decision"].clone())
}

fn pages(out: &std::path::Path, did: &str) -> Vec<String> {
    [
        "index.html".to_string(),
        "identity.html".to_string(),
        format!("_decision/{did}.html"),
    ]
    .iter()
    .map(|p| std::fs::read_to_string(out.join(p)).unwrap())
    .collect()
}

#[test]
fn every_page_carries_the_version_label() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let (doc, did) = small_site(&mut r);
    let render = |r: &mut Rdy, scope: &Scope| {
        let out = tempfile::tempdir().unwrap();
        render_site(r, scope, out.path()).unwrap();
        out
    };
    let out = render(&mut r, &Scope::Canon);
    for page in pages(out.path(), &did) {
        assert!(
            page.contains("<span class=\"version\">no version yet</span>"),
            "{page}"
        );
    }
    assert!(pages(out.path(), &did)[0].contains("No versions yet."));

    r.tag(&founder(), "v0.1.0", None, Some("first cut".into()))
        .unwrap();
    let out = render(&mut r, &Scope::Canon);
    for page in pages(out.path(), &did) {
        assert!(
            page.contains("<span class=\"version\">v0.1.0</span>"),
            "{page}"
        );
    }
    let index = &pages(out.path(), &did)[0];
    assert!(
        index.contains("<span class=\"version-name\">v0.1.0</span>"),
        "{index}"
    );
    assert!(
        index.contains("first cut") && index.contains("<span class=\"badge\">current</span>"),
        "{index}"
    );

    r.write(
        &agent(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(json!([
                {"op": "update", "id": doc, "fields": {"title": "Identity v2"}}
            ]))
            .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let out = render(&mut r, &Scope::View("work".into()));
    assert!(
        pages(out.path(), &did)[1]
            .contains("<span class=\"version\">v0.1.0 · view work, 1 unlanded change</span>")
    );
    let p = r.propose(&agent(), "work", "rename").unwrap();
    r.land(&founder(), &p.id).unwrap();
    let out = render(&mut r, &Scope::Canon);
    assert!(
        pages(out.path(), &did)[2].contains("<span class=\"version\">v0.1.0 + 1 landing</span>")
    );

    let at = r.resolve_scope("tag:v0.1.0").unwrap();
    let out = render(&mut r, &at);
    for page in pages(out.path(), &did) {
        assert!(
            page.contains("<span class=\"version frozen\">v0.1.0 · frozen snapshot</span>"),
            "{page}"
        );
    }
    assert!(
        pages(out.path(), &did)[1].contains("<title>Identity ·"),
        "the snapshot shows the old title"
    );
}

/// glossary (doc), wallets/ { transactions (doc), balance (doc), cards/ { limits (doc) } }
fn site_tree(r: &mut Rdy) -> serde_json::Map<String, serde_json::Value> {
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$g", "fields": {"title": "Glossary", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$gs", "parent": "$g", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$term", "parent": "$gs", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "folder", "id": "$w", "fields": {"name": "wallets"}},
        {"op": "create", "type": "doc", "id": "$t", "parent": "$w", "fields": {"title": "Transactions", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$ts", "parent": "$t", "fields": {"title": "Terms"}},
        {"op": "create", "type": "claim", "id": "$claim", "parent": "$ts", "body": "Posts to the {{$term}}."},
        {"op": "create", "type": "doc", "id": "$b", "parent": "$w", "fields": {"title": "Balance", "doc_type": "glossary"}},
        {"op": "create", "type": "folder", "id": "$c", "parent": "$w", "fields": {"name": "cards", "description": "Card products."}},
        {"op": "create", "type": "doc", "id": "$l", "parent": "$c", "fields": {"title": "Limits", "doc_type": "glossary"}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let p = r.propose(&agent(), "work", "tree").unwrap();
    r.land(&founder(), &p.id).unwrap();
    serde_json::to_value(res.created)
        .unwrap()
        .as_object()
        .unwrap()
        .clone()
}

#[test]
fn the_site_mirrors_the_folder_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let ids = site_tree(&mut r);
    let id = |k: &str| ids[k].as_str().unwrap().to_string();
    let out = tempfile::tempdir().unwrap();
    let s = render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    for p in [
        "index.html",
        "glossary.html",
        "wallets/index.html",
        "wallets/transactions.html",
        "wallets/balance.html",
        "wallets/cards/index.html",
        "wallets/cards/limits.html",
    ] {
        assert!(out.path().join(p).exists(), "{p}");
    }
    assert_eq!(s.pages, 7);
    let read = |p: &str| std::fs::read_to_string(out.path().join(p)).unwrap();

    let t = read("wallets/transactions.html");
    assert!(t.contains("<nav class=\"crumbs\"><a href=\"../index.html\">rdy</a> / <a href=\"../wallets/index.html\">wallets</a> / <span class=\"here\">Transactions</span>"), "{t}");
    assert!(
        t.contains(&format!(
            "href=\"../glossary.html#{}\">Ledger</a>",
            id("$term")
        )),
        "relative link across folders: {t}"
    );
    let sidebar = "<aside class=\"tree\"><ul><li><a href=\"../glossary.html\">Glossary</a></li><li><a href=\"../wallets/index.html\">wallets</a><ul><li class=\"current\"><a href=\"../wallets/transactions.html\">Transactions</a></li><li><a href=\"../wallets/balance.html\">Balance</a></li><li><a href=\"../wallets/cards/index.html\">cards</a></li></ul></li></ul></aside>";
    assert!(t.contains(sidebar), "neighbourhood sidebar: {t}");
    assert!(
        !t.contains("limits.html"),
        "a cousin folder's contents are not shown"
    );

    let cards = read("wallets/cards/index.html");
    assert!(cards.contains("Card products."), "{cards}");
    assert!(
        cards.contains(
            "<a href=\"../../wallets/cards/limits.html\">Limits</a> <span class=\"meta\">doc</span>"
        ),
        "{cards}"
    );
    assert!(cards.contains("<li class=\"current\"><a href=\"../../wallets/cards/index.html\">cards</a><ul><li><a href=\"../../wallets/cards/limits.html\">Limits</a></li></ul></li>"), "the current folder's children: {cards}");

    let index = read("index.html");
    assert!(index.contains("<span class=\"here\">rdy</span>"));
    assert!(
        index.contains("<a href=\"wallets/cards/limits.html\">Limits</a>"),
        "the index lists the whole tree: {index}"
    );
    assert!(!index.contains("<aside"));
}

#[test]
fn decisions_and_assets_live_under_underscore_directories() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let blob = r.store_mut().put_blob(b"\x89PNG fake").unwrap().to_string();
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "folder", "id": "$w", "fields": {"name": "wallets"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$w", "fields": {"title": "Design", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}},
        {"op": "create", "type": "figure", "parent": "$s", "fields": {"blob": blob, "media_type": "image/png", "alt": "logo"}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let d = r
        .decide(
            &agent(),
            "work",
            DecisionInput {
                parent: res.created["$s"].clone(),
                statement: "Ledger is append-only".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let did = d.created["$decision"].clone();
    let p = r.propose(&agent(), "work", "seed").unwrap();
    r.land(&founder(), &p.id).unwrap();
    let out = tempfile::tempdir().unwrap();
    render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let doc = std::fs::read_to_string(out.path().join("wallets/design.html")).unwrap();
    assert!(doc.contains(&format!("src=\"../_assets/{blob}\"")), "{doc}");
    assert!(
        doc.contains(&format!("href=\"../_decision/{did}.html\"")),
        "{doc}"
    );
    assert!(out.path().join(format!("_assets/{blob}")).exists());
    let dec = std::fs::read_to_string(out.path().join(format!("_decision/{did}.html"))).unwrap();
    assert!(dec.contains(&format!("<a href=\"../wallets/design.html\">Design</a> / <span class=\"here\">decision {did}</span>")), "{dec}");
}

#[test]
#[ignore]
fn site_size_at_1k_folders() {
    // cargo test -p rdy-render --release --test render site_size_at_1k_folders -- --ignored --nocapture
    let dir = tempfile::tempdir().unwrap();
    rdy_engine::testkit::generate_tree(dir.path(), 1000, 10, 8);
    let mut r = Rdy::open(dir.path()).unwrap();
    let out = tempfile::tempdir().unwrap();
    let t = std::time::Instant::now();
    let s = render_site(&mut r, &Scope::View("work".into()), out.path()).unwrap();
    let took = t.elapsed();
    fn size(p: &std::path::Path) -> u64 {
        std::fs::read_dir(p)
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                if p.is_dir() {
                    size(&p)
                } else {
                    p.metadata().unwrap().len()
                }
            })
            .sum()
    }
    println!(
        "site: {} pages, {} bytes, rendered in {took:?} (spec 11 full render target < 2 s)",
        s.pages,
        size(out.path())
    );
}

#[test]
fn a_damaged_store_with_a_very_deep_chain_renders_without_overflowing() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut ops = vec![];
    let mut parent: Option<BlockId> = None;
    for i in 0..5000 {
        let id = BlockId::parse(&format!("fd-{i:05}")).unwrap();
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("name".to_string(), FieldValue::Str("f".into()));
        ops.push(Op::Create {
            block: Block {
                id: id.clone(),
                block_type: "folder".into(),
                parent: parent.clone(),
                order: "V".into(),
                fields,
                body: String::new(),
                links: Default::default(),
                pins: Default::default(),
            },
        });
        parent = Some(id);
    }
    r.store_mut()
        .commit_to_view(
            "work",
            Change {
                view: "work".into(),
                ops,
                deps: vec![],
                provenance: agent().provenance(),
                justification: None,
                message: "damaged".into(),
            },
        )
        .unwrap();
    r.invalidate();
    let out = tempfile::tempdir().unwrap();
    let s = render_site(&mut r, &Scope::View("work".into()), out.path()).unwrap();
    // index + one page per folder down to one past the limit
    assert_eq!(s.pages, 1 + tree::MAX_FOLDER_DEPTH + 1, "{s:?}");
}

#[test]
fn the_sidebar_shows_at_most_50_siblings_per_list() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let ops: Vec<_> = (0..120)
        .map(|i| json!({"op": "create", "type": "doc", "fields": {"name": format!("doc-{i:03}"), "title": format!("Doc {i:03}"), "doc_type": "glossary"}}))
        .collect();
    r.write(
        &agent(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(json!(ops)).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    render_site(&mut r, &Scope::View("work".into()), out.path()).unwrap();
    let page = std::fs::read_to_string(out.path().join("doc-100.html")).unwrap();
    let start = page.find("<aside class=\"tree\">").unwrap();
    let end = page[start..].find("</aside>").unwrap() + start;
    let sidebar = &page[start..end];
    assert!(sidebar.matches("<li").count() <= 51, "{sidebar}");
    assert!(
        sidebar.contains("<li class=\"current\"><a href=\"doc-100.html\">Doc 100</a>"),
        "{sidebar}"
    );
    assert!(
        sidebar.contains("<li class=\"more\"><a href=\"index.html\">+70 more</a></li>"),
        "{sidebar}"
    );
    assert!(
        sidebar.contains("Doc 099") && sidebar.contains("Doc 101"),
        "neighbours: {sidebar}"
    );
    // the index still lists everything
    let index = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(index.contains("Doc 000") && index.contains("Doc 119") && !index.contains("more</a>"));
}

#[test]
fn hostile_folder_and_doc_titles_are_escaped_in_tree_pages() {
    let evil = "<script>x</script>\"'&";
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "evil", "title": evil, "description": evil}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "inner", "title": evil, "doc_type": "glossary"}},
        {"op": "create", "type": "section", "parent": "$d", "fields": {"title": evil}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let out = tempfile::tempdir().unwrap();
    render_site(&mut r, &Scope::View("work".into()), out.path()).unwrap();
    for page in ["evil/index.html", "evil/inner.html", "index.html"] {
        let html = std::fs::read_to_string(out.path().join(page)).unwrap();
        assert!(!html.contains("<script>x"), "{page}: {html}");
        assert!(!html.contains("\"'&"), "{page}: {html}");
        assert!(html.contains("&lt;script&gt;x"), "{page}: {html}");
    }
    let inner = std::fs::read_to_string(out.path().join("evil/inner.html")).unwrap();
    let part = |open: &str, close: &str| {
        let a = inner.find(open).unwrap();
        inner[a..a + inner[a..].find(close).unwrap()].to_string()
    };
    for (what, region) in [
        ("crumbs", part("<nav class=\"crumbs\">", "</nav>")),
        ("sidebar", part("<aside class=\"tree\">", "</aside>")),
    ] {
        assert_eq!(
            region.matches("&lt;script&gt;x").count(),
            2,
            "{what} shows the folder and the doc, escaped: {region}"
        );
    }
}
