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
        kind: rdy_engine::SessionKind::Human,
    }
}
fn founder() -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        kind: rdy_engine::SessionKind::Human,
    }
}

#[test]
fn text_html_escapes_then_links() {
    let t = BlockId::parse("t-MEDGR").unwrap();
    let html = text_html("<b>x</b> & {{t-MEDGR}} {{t-N0PE1}}\nnext", &|id| {
        (id == &t).then(|| rdy_render::Link {
            href: "doc/d-AAAAA.html#t-MEDGR".into(),
            label: "Ledger".into(),
            title: String::new(),
            ..Default::default()
        })
    });
    assert_eq!(
        html,
        "&lt;b&gt;x&lt;/b&gt; &amp; <a href=\"doc/d-AAAAA.html#t-MEDGR\">Ledger</a> <span class=\"ref unresolved\" title=\"This reference could not be resolved\">unresolved reference</span><br>next"
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
    r.decide(
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
    let summary = render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    assert_eq!(summary.pages, 3); // index, one doc and the glossary: a decision has no page
    assert_eq!(summary.assets, 1);

    let t_id = &res.created["$t"];
    let doc = std::fs::read_to_string(out.path().join("identity.html")).unwrap();
    assert!(doc.contains("<title>Identity"));
    assert!(
        doc.contains("href=\"#ledger\" title=\"Identity › Components\">Ledger</a>"),
        "same-doc reference links to the anchor: {doc}"
    );
    assert!(
        doc.contains("id=\"ledger\"")
            && doc.contains(&format!("<span id=\"{t_id}\" class=\"id-anchor\""))
    );
    assert!(doc.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!doc.contains("<script>alert(1)"));
    assert!(doc.contains("<pre class=\"mermaid\">graph TD\n  A --&gt; B</pre>"));
    assert!(
        doc.contains("assets/js/mermaid.min.js") && !doc.contains("cdn"),
        "the page with a diagram loads the vendored library"
    );
    assert!(doc.contains(&format!("src=\"assets/{blob}.png\"")) && doc.contains("alt=\"logo\""));
    assert!(!out.path().join("_decision").exists() && !doc.contains("_decision"));

    let index = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(index.contains(&"identity.html".to_string()));
    assert!(
        !index.contains("mermaid"),
        "no diagram script on pages without diagrams"
    );
    assert!(out.path().join(format!("assets/{blob}.png")).exists());
    assert!(out.path().join("assets/js/mermaid.min.js").exists());
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
        links: ChangeLinks::default(),
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
    let summary = render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    assert_eq!(summary.assets, 0);
    let doc = std::fs::read_to_string(out.path().join("assets.html")).unwrap();
    assert!(doc.contains("<p class=\"figure-missing\">gone (image not available)</p>"));
    assert!(doc.contains("<p class=\"figure-missing\">junk (image not available)</p>"));
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
    r.decide(
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
    render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let doc = std::fs::read_to_string(out.path().join("t.html")).unwrap();
    let index = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    for page in [&doc, &index] {
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
        "Write {{literal}} or <span class=\"ref unresolved\" title=\"This reference could not be resolved\">unresolved reference</span>"
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
    render_site(&r.read(), &Scope::View("work".into()), out.path()).unwrap();
    let doc = std::fs::read_to_string(out.path().join("flows.html")).unwrap();
    assert!(
        doc.contains(
            "<pre class=\"mermaid\">graph TD\n  A{{Decide?}} --&gt; B[Ledger &amp; co]</pre>"
        ),
        "{doc}"
    );
    // Code blocks are highlighted at build time (Docs experience Task 9), so the verbatim check
    // compares the visible text with the markup stripped, not the raw HTML.
    let visible = {
        let mut out = String::new();
        let mut in_tag = false;
        for c in doc.chars() {
            match c {
                '<' => in_tag = true,
                '>' if in_tag => in_tag = false,
                _ if !in_tag => out.push(c),
                _ => {}
            }
        }
        out
    };
    assert!(
        visible.contains(r"x: \{{ .Values.image }}"),
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
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "claim", "id": "$c", "parent": s, "body": format!("Corrections are\nnew entries per {{{{{did}}}}}.")}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let claim = res.created["$c"].clone();
    r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "claim", "parent": s, "body": format!("Compare {{{{{claim}}}}}.")}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let p = r.propose(&agent(), "work", "seed").unwrap();
    r.land(&founder(), &p.id).unwrap();
    let out = tempfile::tempdir().unwrap();
    render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let page = std::fs::read_to_string(out.path().join("ledger.html")).unwrap();
    assert!(
        page.contains(">Corrections are new entries per Ledger is append-only.</a>"),
        "a link's label is the target's text on one line, its references shown as text: {page}"
    );
    assert!(
        page.contains(
            "new entries per <a href=\"#s\" title=\"Ledger › S\">Ledger is append-only</a>."
        ),
        "a reference to a decision leads to the section it sits in: {page}"
    );
}

/// terms (doc), wallets/ { transactions (doc), balance (doc), cards/ { limits (doc) } }
fn site_tree(r: &mut Rdy) -> serde_json::Map<String, serde_json::Value> {
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$g", "fields": {"title": "Terms", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$gs", "parent": "$g", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$term", "parent": "$gs", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "folder", "id": "$w", "fields": {"name": "wallets"}},
        {"op": "create", "type": "doc", "id": "$t", "parent": "$w", "fields": {"title": "Transactions", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$ts", "parent": "$t", "fields": {"title": "Terms"}},
        {"op": "create", "type": "claim", "id": "$claim", "parent": "$ts", "body": "Posts to the {{$term}}."},
        {"op": "create", "type": "doc", "id": "$b", "parent": "$w", "fields": {"title": "Balance", "doc_type": "page"}},
        {"op": "create", "type": "folder", "id": "$c", "parent": "$w", "fields": {"name": "cards", "description": "Card products."}},
        {"op": "create", "type": "doc", "id": "$l", "parent": "$c", "fields": {"title": "Limits", "doc_type": "page"}}
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
    let _ids = site_tree(&mut r);
    let out = tempfile::tempdir().unwrap();
    let s = render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    for p in [
        "index.html",
        "terms.html",
        "glossary.html",
        "wallets/index.html",
        "wallets/transactions.html",
        "wallets/balance.html",
        "wallets/cards/index.html",
        "wallets/cards/limits.html",
    ] {
        assert!(out.path().join(p).exists(), "{p}");
    }
    assert_eq!(s.pages, 8);
    let read = |p: &str| std::fs::read_to_string(out.path().join(p)).unwrap();

    let name = r.read().project_name();
    let t = read("wallets/transactions.html");
    let sep = "<span class=\"crumb-sep\" aria-hidden=\"true\"></span>";
    assert!(t.contains(&format!("<nav class=\"crumbs\" aria-label=\"Breadcrumb\"><a href=\"../index.html\">{name}</a>{sep}<a href=\"../wallets/index.html\">Wallets</a>{sep}<span aria-current=\"page\">Transactions</span>")), "{t}");
    assert!(
        t.contains("href=\"../terms.html#ledger\" title=\"Terms › Terms\">Ledger</a>"),
        "relative link across folders: {t}"
    );
    assert!(
        !t.contains("limits.html"),
        "a cousin folder's contents are not shown"
    );

    let cards = read("wallets/cards/index.html");
    assert!(cards.contains("Card products."), "{cards}");
    assert!(
        cards.contains(
            "<li class=\"child-doc\"><a href=\"../../wallets/cards/limits.html\">Limits</a></li>"
        ),
        "{cards}"
    );

    let index = read("index.html");
    assert!(index.contains(&format!("<span aria-current=\"page\">{name}</span>")));
    assert!(
        index.contains("<a class=\"card\" href=\"wallets/index.html\"><strong>Wallets</strong>"),
        "the home page has a card for each top-level entry: {index}"
    );
    assert!(
        !index.contains("limits.html"),
        "the home page does not list the whole tree: {index}"
    );
}

#[test]
fn url1_assets_live_under_assets_and_decisions_have_no_page() {
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
    render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let doc = std::fs::read_to_string(out.path().join("wallets/design.html")).unwrap();
    assert!(
        doc.contains(&format!("src=\"../assets/{blob}.png\"")),
        "{doc}"
    );
    assert!(out.path().join(format!("assets/{blob}.png")).exists());
    assert!(!out.path().join("_assets").exists() && !out.path().join("_decision").exists());
    assert!(!doc.contains("_decision") && !doc.contains(&format!("{did}.html")));
    // the decision is reached through its permalink, which lands on the text it sits in
    let go = std::fs::read_to_string(out.path().join(format!("go/{did}.html"))).unwrap();
    assert!(go.contains("url=../wallets/design.html#decisions"), "{go}");
}

#[test]
#[ignore]
fn site_size_at_1k_folders() {
    // cargo test -p rdy-render --release --test render site_size_at_1k_folders -- --ignored --nocapture
    let dir = tempfile::tempdir().unwrap();
    rdy_engine::testkit::generate_tree(dir.path(), 1000, 10, 8);
    let r = Rdy::open(dir.path()).unwrap();
    let out = tempfile::tempdir().unwrap();
    let t = std::time::Instant::now();
    let s = render_site(&r.read(), &Scope::View("work".into()), out.path()).unwrap();
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
                links: ChangeLinks::default(),
            },
        )
        .unwrap();
    r.invalidate();
    let out = tempfile::tempdir().unwrap();
    let s = render_site(&r.read(), &Scope::View("work".into()), out.path()).unwrap();
    // index + one page per folder down to one past the limit
    assert_eq!(s.pages, 1 + tree::MAX_FOLDER_DEPTH + 1, "{s:?}");
}

#[test]
fn the_home_page_lists_every_doc_of_a_wide_folder() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let ops: Vec<_> = (0..120)
        .map(|i| json!({"op": "create", "type": "doc", "fields": {"name": format!("doc-{i:03}"), "title": format!("Doc {i:03}"), "doc_type": "page"}}))
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
    render_site(&r.read(), &Scope::View("work".into()), out.path()).unwrap();
    // the home page lists everything (the sidebar's 50-entry cap is the nav's, and tested there)
    let index = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    let main = &index[index.find("<main").unwrap()..index.find("</main>").unwrap()];
    assert!(main.contains("Doc 000") && main.contains("Doc 119") && !main.contains("more</a>"));
}

#[test]
fn hostile_folder_and_doc_titles_are_escaped_in_tree_pages() {
    let evil = "<script>x</script>\"'&";
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "evil", "title": evil, "description": evil}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "inner", "title": evil, "doc_type": "page"}},
        {"op": "create", "type": "section", "parent": "$d", "fields": {"title": evil}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let out = tempfile::tempdir().unwrap();
    render_site(&r.read(), &Scope::View("work".into()), out.path()).unwrap();
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
    let crumbs = part("<nav class=\"crumbs\" aria-label=\"Breadcrumb\">", "</nav>");
    assert_eq!(
        crumbs.matches("&lt;script&gt;x").count(),
        2,
        "the crumbs show the folder and the doc, escaped: {crumbs}"
    );
}

#[test]
fn code_and_diagram_blocks_are_labelled_by_kind_and_number_never_by_their_source() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Grammar", "doc_type": "components"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Rules"}},
        {"op": "create", "type": "code", "id": "$c1", "parent": "$s", "fields": {"lang": "python"}, "body": "match_stmt = 'match' subject_expr"},
        {"op": "create", "type": "code", "id": "$c2", "parent": "$s", "fields": {"lang": "python"}, "body": "def quit_game(): print('goodbye')"},
        {"op": "create", "type": "code", "id": "$c3", "parent": "$s", "fields": {"lang": ""}, "body": "raw_text here"},
        {"op": "create", "type": "diagram", "id": "$d1", "parent": "$s", "fields": {"lang": "mermaid", "alt": "Order flow"}, "body": "graph TD\n  order_in --> order_out"},
        {"op": "create", "type": "diagram", "id": "$d2", "parent": "$s", "fields": {"lang": "mermaid", "alt": ""}, "body": "graph TD\n  left_side --> right_side"},
        {"op": "create", "type": "diagram", "id": "$d3", "parent": "$s", "fields": {"lang": "mermaid", "alt": "flow of a turn"}, "body": "graph TD\n  turn_a --> turn_b"}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let out = tempfile::tempdir().unwrap();
    render_site(&r.read(), &Scope::View("work".into()), out.path()).unwrap();
    let go = |k: &str| {
        let id = &res.created[k];
        std::fs::read_to_string(out.path().join(format!("go/{id}.html"))).unwrap()
    };
    let cases = [
        ("$c1", "Python code 1", "grammar.html#python-code-1"),
        ("$c2", "Python code 2", "grammar.html#python-code-2"),
        ("$c3", "Code 3", "grammar.html#code-3"),
        ("$d1", "Order flow", "grammar.html#order-flow"),
        ("$d2", "Diagram 2", "grammar.html#diagram-2"),
        ("$d3", "Flow of a turn", "grammar.html#flow-of-a-turn"),
    ];
    for (k, label, url) in cases {
        let page = go(k);
        // the title names no block: a summary there would be chrome the scan reads (fix round 1)
        assert!(page.contains("<title>Page moved · "), "{k}: {page}");
        assert!(page.contains(&format!(">{label}</a>")), "{k}: {page}");
        assert!(page.contains(url), "{k}: {page}");
        for leak in [
            "match_stmt",
            "quit_game",
            "raw_text",
            "order_in",
            "left_side",
            "turn_a",
        ] {
            assert!(!page.contains(leak), "{k} leaks {leak}: {page}");
        }
    }
    let doc = std::fs::read_to_string(out.path().join("grammar.html")).unwrap();
    for leak in [
        "match-stmt",
        "match_stmt-",
        "quit-game",
        "raw_text-",
        "graph-td",
    ] {
        assert!(
            !doc.contains(&format!("id=\"{leak}")),
            "anchor from source: {leak}"
        );
    }
}
