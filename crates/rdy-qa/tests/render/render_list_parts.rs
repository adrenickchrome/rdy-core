//! rfr3 (D-7M4TR): a list item in a prose body holds more than one part. After a blank line, a
//! line indented past the item's marker starts a new paragraph of the item; a line indented past
//! the marker that starts with three or more backticks or tildes opens fenced code, which the
//! same fence (at least as long) closes, or the end of the item. Code text is literal in every
//! reader: no reference and no markup is read in it. A fence outside a list item stays text.
use rdy_engine::{Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::fixtures::{land, write};
use rdy_qa::golden::{html, tiny};
use rdy_render::inline::{self, Ctx};
use rdy_render::print::fonts::Fonts;
use rdy_render::print::layout::LItem;
use rdy_render::print::md::{Layout, write_markdown};
use rdy_render::print::{self, ExportLayout, ExportOptions, Format, PBlock, Print};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The parts body: a numbered list with a paragraph after a blank line, backtick and tilde
/// fences with and without a language, a longer closing fence, an unclosed fence that the next
/// item ends, a nested list after a paragraph, then a top-level fence that stays text.
const PARTS: &str = r#"1. **Make a project.**

   ```sh
   rdy init ~/p && cd ~/p
   ```

2. Second step
   continues here.

   A paragraph of its own.

   ~~~~json
   {"a": "{{REF}}", "b": "*not em* <b>"}
   ~~~
   ~~~~~
3. Unclosed:
   ```
     indented two more
   - not a marker
4. Four
   - nested

   After the nested list.

```sh
top level stays text
```"#;

fn link(_: &BlockId) -> Option<rdy_render::Link> {
    None
}

fn body(src: &str) -> String {
    inline::body_html(
        src,
        &Ctx {
            link: &link,
            doc_page: "",
            unavailable: inline::UNRESOLVED,
            local_link: None,
        },
    )
}

/// The code blocks of a piece of the renderer's HTML: each language class and its text, decoded.
fn codes(h: &str) -> Vec<(Option<String>, String)> {
    let re = regex::Regex::new(
        r#"(?s)<pre class="code"><code(?: class="language-([^"]+)")?>(.*?)</code></pre>"#,
    )
    .unwrap();
    let tag = regex::Regex::new(r"<[^>]+>").unwrap();
    re.captures_iter(h)
        .map(|c| {
            let text = tag.replace_all(&c[2], "").to_string();
            let text = text
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&#39;", "'")
                .replace("&amp;", "&");
            (c.get(1).map(|m| m.as_str().to_string()), text)
        })
        .collect()
}

#[test]
fn rfr3_list_parts_keep_paragraphs_code_and_nested_order() {
    let h = body(&PARTS.replace("REF", "d-ZZZZZ"));
    // three lists: the numbered one (four items), the nested one, and none at the top level for
    // the fence that stays text
    assert_eq!(h.matches("<ol>").count(), 1, "{h}");
    assert_eq!(h.matches("<ul>").count(), 1, "{h}");
    // the marker line stays the item's text; a paragraph after a blank line is its own, while a
    // line with no blank line before it still joins with a break
    assert!(
        h.contains("<li>Second step<br>continues here.<p>A paragraph of its own.</p>"),
        "{h}"
    );
    assert!(h.contains("<p>After the nested list.</p></li></ol>"), "{h}");
    // the parts in their written order: text, nested list, then the paragraph
    let four = h.find("<li>Four").expect("item four");
    let nested = h.find("<ul><li>nested</li></ul>").expect("the nested list");
    let after = h
        .find("<p>After the nested list.</p>")
        .expect("the paragraph");
    assert!(four < nested && nested < after, "{h}");
    // backticks and tildes, a language or none, a longer closing fence, the end of an item
    assert_eq!(
        codes(&h),
        vec![
            (Some("sh".into()), "rdy init ~/p && cd ~/p".into()),
            (
                Some("json".into()),
                "{\"a\": \"{{d-ZZZZZ}}\", \"b\": \"*not em* <b>\"}\n~~~".into()
            ),
            (None, "  indented two more\n- not a marker".into()),
        ],
        "{h}"
    );
    // each code part sits in its item, before the next item starts
    let first = h.find("language-sh").unwrap();
    let second_item = h.find("<li>Second step").unwrap();
    assert!(first < second_item, "{h}");
    // a fence outside a list item stays text: as before, a code span of its inline markup
    assert!(
        h.ends_with("</ol><p><code>sh<br>top level stays text<br></code></p>"),
        "{h}"
    );
    // the old forms keep their bytes (md1's guards): a line with no blank line joins
    assert_eq!(
        body("- item\n  continues here\n- next"),
        "<ul><li>item<br>continues here</li><li>next</li></ul>"
    );
    assert_eq!(
        body("- a\n\nNot indented."),
        "<ul><li>a</li></ul><p>Not indented.</p>"
    );
    // the changed form: an indented paragraph after a blank line was joined, and is now its own
    assert_eq!(
        body("- a\n\n  b"),
        "<ul><li>a<p>b</p></li></ul>",
        "D-7M4TR rule 5"
    );
    // a tab counts as four columns, and a fence's indent is dropped from each line
    assert_eq!(
        codes(&body("- a\n\t```\n\tx\n\t  y\n\t```")),
        vec![(None, "x\n  y".into())]
    );
    // a backtick fence whose info holds a backtick is text, as a code span would read it
    assert!(
        codes(&body("- a\n  ``` x`y\n  z")).is_empty(),
        "no fence: {}",
        body("- a\n  ``` x`y\n  z")
    );

    // unchanged hashes: the stored body is the bytes written, so the content hash of a block
    // with these forms is the hash of the same block written again
    let (_d, mut r) = tiny();
    let (ids, _) = parts_project(&mut r);
    let st = r.read();
    let m = st.materialized(&Scope::Canon).unwrap();
    let p = m.get(&ids["$p"]).unwrap();
    let written = PARTS.replace("REF", &ids["$q"].to_string());
    assert_eq!(p.body, written, "the body is stored as written");
    let again = rdy_model::Block {
        body: written.clone(),
        ..p.clone()
    };
    assert_eq!(again.content_hash(), p.content_hash());
}

fn parts_project(r: &mut Rdy) -> (BTreeMap<String, BlockId>, ()) {
    let w = write(
        r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "steps", "title": "Steps", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$t", "parent": "$d", "fields": {"title": "Target"}},
            {"op": "create", "type": "prose", "id": "$q", "parent": "$t", "body": "The target block."},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Try it"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": PARTS.replace("REF", "$q")},
        ]),
    )
    .unwrap_or_else(|e| panic!("write: {e}"));
    land(r, "w1").unwrap();
    (
        w.created
            .iter()
            .map(|(k, v)| (k.clone(), BlockId::parse(v).unwrap()))
            .collect(),
        (),
    )
}

/// Every file under `dir`, by its `/` path.
fn tree(dir: &std::path::Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(t) = std::fs::read_to_string(&p) {
                let rel: Vec<_> = p
                    .strip_prefix(dir)
                    .unwrap()
                    .iter()
                    .map(|c| c.to_string_lossy().into_owned())
                    .collect();
                out.insert(rel.join("/"), t);
            }
        }
    }
    out
}

/// The code blocks of the print's list items, in page order.
fn item_codes(blocks: &[PBlock], in_item: bool, out: &mut Vec<(Option<String>, String)>) {
    for b in blocks {
        match b {
            PBlock::Code { lang, text, .. } if in_item => out.push((lang.clone(), text.clone())),
            PBlock::List { items, .. } => items.iter().for_each(|i| item_codes(i, true, out)),
            PBlock::Callout { blocks, .. } | PBlock::Fold { blocks, .. } => {
                item_codes(blocks, in_item, out)
            }
            _ => {}
        }
    }
}

fn pdf_words(p: &Print) -> String {
    print::pdf::lay_out(p, &Fonts::load())
        .pages
        .iter()
        .flat_map(|page| page.items.iter())
        .filter_map(|it| match it {
            LItem::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The Copy payload's text parts of a page, joined: what Copy writes, with its link slots left out.
fn copy_text(page: &str) -> String {
    let re = regex::Regex::new(r"(?s)<template data-copy-page hidden>(.*?)</template>").unwrap();
    let inner = &re.captures(page).expect("a Copy template")[1];
    let json = inner
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    let v: Value = serde_json::from_str(&json).unwrap();
    v["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            b.as_array()
                .unwrap()
                .iter()
                .filter(|p| p["kind"] == "text")
                .map(|p| p["value"].as_str().unwrap().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn rfr3_list_code_is_literal_in_every_reader() {
    let (_d, mut r) = tiny();
    let (ids, _) = parts_project(&mut r);
    let st = r.read();
    let rf = format!("{{{{{}}}}}", ids["$q"]);
    let json_code = format!("{{\"a\": \"{rf}\", \"b\": \"*not em* <b>\"}}\n~~~");

    // the site: the code block's body, literal; the ref in it is no link and no marker
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&st, &Scope::Canon, out.path()).unwrap();
    let page = html(out.path(), "steps.html");
    let shown = page.split("<template data-copy-page").next().unwrap();
    let found = codes(shown);
    assert!(
        found.contains(&(Some("json".into()), json_code.clone())),
        "{found:?}"
    );
    assert!(!shown.contains("ref unresolved"), "no ref is read in code");
    assert!(
        !shown.contains("<em>not em</em>"),
        "no markup is read in code"
    );

    // the print model: each code part is a code block inside its item
    let p = print::build(&st, &Scope::Canon).unwrap();
    let doc = p.docs.iter().find(|d| d.page == "steps.html").unwrap();
    let mut got = vec![];
    item_codes(&doc.blocks, false, &mut got);
    assert_eq!(
        got,
        vec![
            (Some("sh".into()), "rdy init ~/p && cd ~/p".into()),
            (Some("json".into()), json_code.clone()),
            (None, "  indented two more\n- not a marker".into()),
        ]
    );
    // the PDF shows the code's text
    let words = pdf_words(&p);
    assert!(words.contains("rdy init ~/p && cd ~/p"), "{words}");
    assert!(words.contains(&rf), "{words}");

    // the single HTML file: the code block's body, in the item
    let h = String::from_utf8(print::single::single_html(&p)).unwrap();
    assert!(h.contains("<li>"), "{h}");
    assert!(h.contains(&rf), "{h}");
    assert!(h.contains("*not em* &lt;b&gt;"), "{h}");

    // the Markdown tree: each part indented to the item's text, a blank line between parts
    let dir = tempfile::tempdir().unwrap();
    for layout in [Layout::Plain, Layout::Mkdocs, Layout::Techdocs] {
        let md = dir.path().join(layout.name());
        write_markdown(&p, &md, layout).unwrap();
        let files = tree(&md);
        let file = files
            .iter()
            .find(|(k, _)| k.ends_with("steps.md"))
            .map(|(_, v)| v.clone())
            .unwrap();
        assert!(
            file.contains("    ```sh\n    rdy init ~/p && cd ~/p\n    ```\n2.  Second step\\\n"),
            "{layout:?}: {file}"
        );
        assert!(
            file.contains("    A paragraph of its own.\n\n    JSON\n\n    ```json\n"),
            "{layout:?}: {file}"
        );
        assert!(file.contains(&rf), "{layout:?}: {file}");
        assert!(
            file.contains("*not em* <b>"),
            "{layout:?}: literal code: {file}"
        );
    }

    // the single Markdown file: the same parts, aligned to the item's text
    let single = dir.path().join("steps.md");
    let only = print::export_with(
        &st,
        &Scope::Canon,
        &single,
        Format::Md,
        &ExportOptions {
            layout: Some(ExportLayout::Single),
            assets: None,
        },
    )
    .unwrap();
    assert_eq!(only.docs, 1);
    let text = std::fs::read_to_string(&single).unwrap();
    assert!(
        text.contains("1. **Make a project.**\n\n   ```sh\n   rdy init ~/p && cd ~/p\n   ```\n\n2. Second step\n   continues here.\n\n   A paragraph of its own.\n"),
        "{text}"
    );
    assert!(
        text.contains(&rf) && text.contains("*not em* <b>"),
        "{text}"
    );
    // the top-level fence stays text
    assert!(text.ends_with("`sh`\n`top level stays text`\n"), "{text}");

    // Copy follows the same model
    let copy = copy_text(&page);
    assert!(
        copy.contains("1.  **Make a project.**\n\n    Shell\n\n    ```sh\n    rdy init ~/p && cd ~/p\n    ```\n2.  Second step"),
        "{copy}"
    );
    assert!(copy.contains(&rf), "{copy}");

    // search reads the code's literal text; llms reads it fenced
    let index = std::fs::read_to_string(out.path().join("search-index.json")).unwrap();
    assert!(index.contains("rdy init ~/p && cd ~/p"), "{index}");
    assert!(index.contains(&rf), "{index}");
    let full = std::fs::read_to_string(out.path().join("llms-full.txt")).unwrap();
    assert!(full.contains("rdy init ~/p && cd ~/p"), "{full}");
    assert!(full.contains(&rf), "{full}");
}
