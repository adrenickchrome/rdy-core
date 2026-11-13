//! rfr3 (D-ESHXM, D-6PR3Y, D-H1FRY): `rdy export --format md --layout single --out <file>
//! [--assets <dir>]` writes the one doc of the selection as one GitHub Flavored Markdown file,
//! from the print model: no front matter, anchors, captions, labels or pills; minimal escapes;
//! refs inside the doc lead to GitHub's heading slugs and any other ref is its label with a
//! warning; figures are written by the SHA-256 of their bytes into the assets folder. Both
//! destinations are checked before anything is written, and a failure removes only what the call
//! made. The tree layouts keep their contract.
use rdy_engine::{E_ALREADY_EXISTS, E_BAD_INPUT, E_PUBLICATION_REFERENCE, Rdy, RdyError, Scope};
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::{land, write};
use rdy_qa::golden::tiny;
use rdy_render::print::md::Layout;
use rdy_render::print::single_md::{asset_name, with_failed_write_for_tests};
use rdy_render::print::{self, ExportLayout, ExportOptions, Format, PrintSummary};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

// --- helpers ------------------------------------------------------------------------------------

fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, BlockId> {
    let w = write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e}"));
    land(r, view).unwrap_or_else(|e| panic!("land {view}: {e}"));
    w.created
        .iter()
        .map(|(k, v)| (k.clone(), BlockId::parse(v).unwrap()))
        .collect()
}

fn opts(assets: Option<&str>) -> ExportOptions<'_> {
    ExportOptions {
        layout: Some(ExportLayout::Single),
        assets,
    }
}

/// The single file of the source at canon.
fn single(r: &Rdy, out: &Path, assets: Option<&str>) -> Result<PrintSummary, RdyError> {
    print::export_with(&r.read(), &Scope::Canon, out, Format::Md, &opts(assets))
}

/// The single file of `profile` at canon.
fn single_of(
    r: &Rdy,
    profile: &str,
    out: &Path,
    assets: Option<&str>,
) -> Result<PrintSummary, RdyError> {
    print::export_profile_with(&r.read(), "canon", profile, out, Format::Md, &opts(assets))
}

/// Every entry under `dir` (files with their bytes, folders with none), by its `/` path.
fn tree(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(base: &Path, at: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for e in std::fs::read_dir(at).unwrap() {
            let p = e.unwrap().path();
            let rel: Vec<_> = p
                .strip_prefix(base)
                .unwrap()
                .iter()
                .map(|c| c.to_string_lossy().into_owned())
                .collect();
            let md = std::fs::symlink_metadata(&p).unwrap();
            if md.is_dir() {
                out.insert(rel.join("/") + "/", None);
                walk(base, &p, out);
            } else {
                out.insert(rel.join("/"), std::fs::read(&p).ok());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// A generic picture's bytes: a PNG signature and a distinct tail.
fn picture(tail: &str) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    b.extend_from_slice(tail.as_bytes());
    b
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn read(path: &Path) -> String {
    String::from_utf8(std::fs::read(path).unwrap()).unwrap()
}

/// The single file of a doc with no figure, under the folder of `out` (the asset count, the
/// bytes and the warnings come from the summary).
fn names_of(w: &World) -> (String, String, String) {
    (
        asset_name(&w.light, "image/png"),
        asset_name(&w.dark, "image/png"),
        asset_name(&w.plain, "image/png"),
    )
}

// --- the fixture --------------------------------------------------------------------------------

/// The README's six-step shape today: a numbered list, each step a bold line, then its code.
const STEPS: &str = r#"1. **Make a project.**

   ```sh
   rdy init ~/notes && cd ~/notes
   ```

2. **Write a doc.**

   ```sh
   rdy write --view first <<'EOF'
   [{"op": "create", "type": "doc", "id": "$d"}]
   EOF
   ```

3. **Land it.**

   ```sh
   rdy land first
   ```

4. **Read it.**

   ```sh
   rdy show d-XXXXX
   ```

5. **Build the site.**

   ```sh
   rdy site
   ```

6. **Export.**

   ```sh
   rdy export --format md --layout single --out README.md
   ```"#;

/// A project with folder `area` holding doc `Guide` (the single doc of profile `onep`) and doc
/// `Other`, and the profiles `onep` (org, `Guide`), `twop` (org, both docs), `nonep` (org, the
/// folder's sibling with no doc) and `pubp` (public, `Guide`, whose refs outside it block it).
struct World {
    _dir: tempfile::TempDir,
    r: Rdy,
    ids: BTreeMap<String, BlockId>,
    light: Vec<u8>,
    dark: Vec<u8>,
    plain: Vec<u8>,
}

fn world() -> World {
    let (dir, mut r) = tiny();
    let light = picture("light image bytes");
    let dark = picture("dark image bytes");
    let plain = picture("plain image bytes");
    let blobs: Vec<Hash> = [&light, &dark, &plain, &light]
        .iter()
        .map(|b| r.store_mut().put_blob(b).unwrap())
        .collect();
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$area", "fields": {"name": "area", "title": "Area", "description": "The area."}},
            {"op": "create", "type": "doc", "id": "$o", "parent": "$area", "fields": {"name": "other", "title": "Other", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$os", "parent": "$o", "fields": {"title": "Elsewhere"}},
            {"op": "create", "type": "prose", "id": "$op", "parent": "$os", "body": "OTHERCANARY words."},
            {"op": "create", "type": "doc", "id": "$g", "parent": "$area", "fields": {"name": "guide", "title": "Guide & <Tour>", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$intro", "parent": "$g", "fields": {"title": "Intro"}},
            {"op": "create", "type": "prose", "parent": "$intro", "body": "Lede words of the guide."},
            {"op": "create", "type": "section", "id": "$i1", "parent": "$g", "fields": {"title": "Install"}},
            {"op": "create", "type": "prose", "id": "$esc", "parent": "$i1", "body": "Keep E_STORE_FORMAT and snake_case_name, 2 * 3, a_b and https://example.com/x_y as is.\n<b>raw</b> and # not a heading\n> not a quote and &amp; stays."},
            {"op": "create", "type": "code", "id": "$code", "parent": "$i1", "fields": {"lang": "sh"}, "body": "cargo install rdy\n# a comment"},
            {"op": "create", "type": "section", "id": "$i2", "parent": "$g", "fields": {"title": "Install"}},
            {"op": "create", "type": "section", "id": "$uni", "parent": "$i2", "fields": {"title": "Über Größe: 2.0!"}},
            {"op": "create", "type": "prose", "id": "$deep", "parent": "$uni", "body": "Deep words."},
            {"op": "create", "type": "table", "id": "$tb", "parent": "$uni", "fields": {"columns": ["Name", "Count"], "rows": [["a|b", "2"]]}, "body": "The table's body."},
            {"op": "create", "type": "diagram", "id": "$dg", "parent": "$uni", "fields": {"lang": "mermaid", "alt": "A flow"}, "body": "flowchart TD\n  A --> B"},
            {"op": "create", "type": "term", "id": "$t", "parent": "$uni", "fields": {"name": "Guard"}, "body": "A condition."},
            {"op": "create", "type": "section", "id": "$fold", "parent": "$g", "fields": {"title": "Folded", "collapsed": true}},
            {"op": "create", "type": "prose", "id": "$hid", "parent": "$fold", "body": "Folded words."},
            {"op": "create", "type": "section", "id": "$steps", "parent": "$g", "fields": {"title": "Try it"}},
            {"op": "create", "type": "prose", "id": "$st", "parent": "$steps", "body": STEPS},
            {"op": "create", "type": "section", "id": "$links", "parent": "$g", "fields": {"title": "Links"}},
            {"op": "create", "type": "prose", "id": "$refs", "parent": "$links", "body": "See {{$deep}}, {{$uni|the umlaut part}}, {{$i1}}, {{$i2|second}}, {{$hid|folded}}, {{$fold}}, {{$op|other}} and [own](#install) and [Other page](other.html) and [ext](https://example.com/a)."},
            {"op": "create", "type": "section", "id": "$figs", "parent": "$g", "fields": {"title": "Figures"}},
            {"op": "create", "type": "figure", "id": "$fp", "parent": "$figs", "fields": {"blob": blobs[0].to_string(), "media_type": "image/png", "dark_blob": blobs[1].to_string(), "dark_media_type": "image/png", "alt": "Paired"}},
            {"op": "create", "type": "figure", "id": "$fq", "parent": "$figs", "fields": {"blob": blobs[2].to_string(), "media_type": "image/png", "alt": "Plain figure"}},
            {"op": "create", "type": "figure", "id": "$fr", "parent": "$figs", "fields": {"blob": blobs[3].to_string(), "media_type": "image/png", "alt": "Same bytes"}},
            {"op": "create", "type": "prose", "parent": "$figs", "body": "Badge ![Build](https://img.example.com/badge.svg) stays."},
            {"op": "create", "type": "note", "id": "$plan", "parent": "$figs", "fields": {"planned": true}, "body": "PLANNEDCANARY note."},
            {"op": "create", "type": "folder", "id": "$empty", "fields": {"name": "empty", "title": "Empty", "description": "No doc."}}
        ]),
    );
    let g = ids["$g"].to_string();
    let o = ids["$o"].to_string();
    let admin = put(
        &mut r,
        "admin",
        json!([{"op": "create", "type": "doc", "id": "$admin",
            "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}]),
    )["$admin"]
        .to_string();
    let profile = |key: &str, audience: &str, select: Vec<&str>| {
        json!({"op": "create", "type": "publication_profile", "parent": admin,
            "fields": {"key": key, "title": format!("Title of {key}"),
                "description": format!("Description of {key}."), "audience": audience,
                "select": select}})
    };
    let area = ids["$area"].to_string();
    let empty = ids["$empty"].to_string();
    put(
        &mut r,
        "profiles",
        json!([
            profile("onep", "org", vec![&g]),
            json!({"op": "create", "type": "publication_profile", "parent": admin,
                "fields": {"key": "hidep", "title": "Hide", "audience": "org", "planned": "hide", "select": [&g]}}),
            profile("twop", "org", vec![&g, &o]),
            profile("pubp", "public", vec![&g]),
            profile("areap", "org", vec![&area]),
            profile("nonep", "org", vec![&empty]),
        ]),
    );
    World {
        _dir: dir,
        r,
        ids,
        light,
        dark,
        plain,
    }
}

impl World {
    fn id(&self, k: &str) -> String {
        self.ids[k].to_string()
    }
}

/// The bytes `onep`'s single file holds, its images under `assets`.
fn expected(w: &World) -> String {
    let (l, d, p) = names_of(w);
    format!(
        r#"# Guide & \<Tour>

Lede words of the guide.

## Intro

## Install

Keep E_STORE_FORMAT and snake_case_name, 2 * 3, a_b and https://example.com/x_y as is.
\<b>raw\</b> and # not a heading
\> not a quote and \&amp; stays.

```sh
cargo install rdy
# a comment
```

## Install

### Über Größe: 2.0!

Deep words.

| Name | Count |
| --- | --- |
| a\|b | 2 |

The table's body.

```mermaid
flowchart TD
  A --> B
```

**Guard** A condition.

<details>
<summary><b>Folded</b></summary>

Folded words.

</details>

## Try it

{STEPS}

## Links

See [Deep words.](#über-größe-20), [the umlaut part](#über-größe-20), [Install](#install), [second](#install-1), folded, Folded, other and [own](#install) and Other page and [ext](https://example.com/a).

## Figures

<picture>
<source media="(prefers-color-scheme: dark)" srcset="assets/{d}">
<img alt="Paired" src="assets/{l}">
</picture>

![Plain figure](assets/{p})

![Same bytes](assets/{l})

Badge ![Build](https://img.example.com/badge.svg) stays.

> PLANNEDCANARY note.
"#
    )
}

// --- 1. one doc ---------------------------------------------------------------------------------

#[test]
fn rfr3_single_requires_exactly_one_selected_doc() {
    let w = world();
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let wrong = |n: usize, e: RdyError| {
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_BAD_INPUT,
                format!("a single-file Markdown export takes one doc; the selection holds {n}")
                    .as_str(),
                "choose a profile that selects one doc"
            )
        );
    };
    // the full source holds three docs (the guide, the other one and the admin page)
    wrong(3, single(&w.r, &base.join("a.md"), None).unwrap_err());
    wrong(
        3,
        single(&w.r, &base.join("a.md"), Some("assets")).unwrap_err(),
    );
    // two docs by name, two by their folder, none in an empty folder
    wrong(
        2,
        single_of(&w.r, "twop", &base.join("b.md"), Some("assets")).unwrap_err(),
    );
    wrong(
        2,
        single_of(&w.r, "areap", &base.join("c.md"), Some("assets")).unwrap_err(),
    );
    wrong(
        0,
        single_of(&w.r, "nonep", &base.join("d.md"), Some("assets")).unwrap_err(),
    );
    assert!(tree(base).is_empty(), "a refusal writes nothing");
    // one doc: its folder shell and the glossary its term makes are no docs
    let s = single_of(&w.r, "onep", &base.join("e.md"), Some("assets")).unwrap();
    assert_eq!((s.docs, s.folders, s.pages), (1, 0, None));
    // the full source of a project with one doc and a glossary
    let (_d, mut r) = tiny();
    put(
        &mut r,
        "one",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "solo", "title": "Solo", "doc_type": "page"}},
            {"op": "create", "type": "term", "parent": "$d", "fields": {"name": "Word"}, "body": "A word."}
        ]),
    );
    let s = single(&r, &base.join("solo.md"), None).unwrap();
    assert_eq!((s.docs, s.assets), (1, 0));
    assert_eq!(read(&base.join("solo.md")), "# Solo\n\n**Word** A word.\n");
}

// --- 2. the filtered print model ----------------------------------------------------------------

#[test]
fn rfr3_single_uses_the_filtered_print_model() {
    let w = world();
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one.md");
    single_of(&w.r, "onep", &one, Some("assets")).unwrap();
    let text = read(&one);
    // the lede rule: the doc's first words follow its title
    assert!(text.starts_with("# Guide & \\<Tour>\n\nLede words of the guide.\n\n## Intro\n"));
    // a ref outside the selection is its words; nothing of the other doc, the profile or the
    // project reaches the file, and no id or private metadata does either
    for gone in [
        "OTHERCANARY",
        "Title of onep",
        "Description of onep",
        "Admin page",
        "Tiny",
        "rdy_id",
        "---\n",
        "generated",
        "data-rdy-ref",
        "<a id=",
        "Example",
        "Shell",
    ] {
        assert!(!text.contains(gone), "{gone}: {text}");
    }
    for (k, id) in &w.ids {
        assert!(!text.contains(&id.to_string()), "{k} {id}");
    }
    // the planned note is shown with no pill when planned content is marked, and left out when
    // the profile hides it
    assert!(text.contains("PLANNEDCANARY note."));
    assert!(!text.contains("Planned"), "{text}");
    let hid = dir.path().join("hid.md");
    single_of(&w.r, "hidep", &hid, Some("hid-assets")).unwrap();
    assert!(!read(&hid).contains("PLANNEDCANARY"));
    // the same words as the tree's file of the doc, which reads the same model
    let tree_out = dir.path().join("tree");
    print::export_profile(&w.r.read(), "canon", "onep", &tree_out, Format::Md, None).unwrap();
    let doc = std::fs::read_to_string(tree_out.join("area").join("guide.md")).unwrap();
    for line in [
        "Deep words.",
        "The table's body.",
        "Folded words.",
        "PLANNEDCANARY note.",
    ] {
        assert!(doc.contains(line) && text.contains(line), "{line}");
    }
    // an unavailable ref with no display text reads as the site reads it
    let (_d, mut r) = tiny();
    let ids = put(
        &mut r,
        "u",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$pa", "parent": "$a", "body": "A words."},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$pb", "parent": "$b", "body": "See {{$pa}} now."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    put(
        &mut r,
        "p",
        json!([{"op": "create", "type": "publication_profile", "parent": ids["$admin"].to_string(),
            "fields": {"key": "bp", "title": "B only", "audience": "org", "select": [ids["$b"].to_string()]}}]),
    );
    let b = dir.path().join("b.md");
    let s = single_of(&r, "bp", &b, None).unwrap();
    assert_eq!(read(&b), "# B\n\nSee unavailable reference now.\n");
    assert_eq!(
        s.warnings.last().unwrap(),
        &format!(
            "warning: b: {}: the ref to {} is not in this file; it is written as its label",
            ids["$pb"], ids["$pa"]
        )
    );
}

// --- 3. bytes -----------------------------------------------------------------------------------

#[test]
fn rfr3_single_has_exact_gfm_bytes() {
    let w = world();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("README.md");
    let s = single_of(&w.r, "onep", &out, Some("assets")).unwrap();
    let bytes = std::fs::read(&out).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert_eq!(text, expected(&w));
    assert_eq!(s.bytes, bytes.len() as u64);
    assert!(!bytes.contains(&b'\r'), "LF line ends only");
    assert!(
        text.ends_with(".\n") && !text.ends_with("\n\n"),
        "one final newline"
    );
    // the summary
    let v = serde_json::to_value(&s).unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "assets", "bytes", "docs", "folders", "format", "layout", "out", "pages", "warnings"
        ]
    );
    assert_eq!(
        (
            &v["format"],
            &v["layout"],
            &v["docs"],
            &v["folders"],
            &v["assets"],
            &v["pages"]
        ),
        (
            &json!("md"),
            &json!("single"),
            &json!(1),
            &json!(0),
            &json!(3),
            &Value::Null
        )
    );
    assert_eq!(v["out"], out.display().to_string());
    // six heading levels at most: a seventh level is still `######`
    let (_d, mut r) = tiny();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "deep", "title": "Deep", "doc_type": "page"}}),
    ];
    let mut parent = "$d".to_string();
    for n in 1..=7 {
        let id = format!("$s{n}");
        ops.push(json!({"op": "create", "type": "section", "id": id, "parent": parent, "fields": {"title": format!("Level {n}")}}));
        parent = id;
    }
    ops.push(json!({"op": "create", "type": "prose", "parent": parent, "body": "Bottom."}));
    put(&mut r, "deep", Value::Array(ops));
    let deep = dir.path().join("deep.md");
    single(&r, &deep, None).unwrap();
    assert_eq!(
        read(&deep),
        "# Deep\n\n## Level 1\n\n### Level 2\n\n#### Level 3\n\n##### Level 4\n\n###### Level 5\n\n\
         ###### Level 6\n\n###### Level 7\n\nBottom.\n"
    );
}

// --- 4. code in numbered steps ------------------------------------------------------------------

#[test]
fn rfr3_single_preserves_code_inside_numbered_steps() {
    let w = world();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("README.md");
    single_of(&w.r, "onep", &out, Some("assets")).unwrap();
    let text = read(&out);
    // the README's six steps come out as they were written: each step's code in its step
    assert!(
        text.contains(&format!("## Try it\n\n{STEPS}\n\n## Links\n")),
        "{text}"
    );
    for n in 1..=6 {
        let at = text.find(&format!("\n{n}. **")).expect("the step");
        let code = at + text[at..].find("   ```sh\n").expect("its code");
        let next = text[at + 1..]
            .find(&format!("\n{}. **", n + 1))
            .map_or(usize::MAX, |i| at + 1 + i);
        assert!(code < next, "step {n}'s code stays in step {n}");
    }
    // no raw passthrough of the site's markup
    for raw in [
        "<pre", "<code", "<div", "<ol", "<li", "<p>", "&lt;", "&quot;",
    ] {
        assert!(!text.contains(raw), "{raw}");
    }
    // the D-7M4TR authoring form with tildes, a longer close and an unclosed fence
    let (_d, mut r) = tiny();
    put(
        &mut r,
        "t",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "t", "title": "T", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "- One\n\n  ~~~~text\n  a <b> & `c`\n  ~~~~~\n\n  After.\n- Two\n  ```\n  open to the end"}
        ]),
    );
    let t = dir.path().join("t.md");
    single(&r, &t, None).unwrap();
    assert_eq!(
        read(&t),
        "# T\n\n- One\n\n  ```text\n  a <b> & `c`\n  ```\n\n  After.\n\n- Two\n\n  ```\n  open to the end\n  ```\n"
    );
}

// --- 5. links -----------------------------------------------------------------------------------

#[test]
fn rfr3_single_links_use_heading_slugs_or_warn() {
    let w = world();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("README.md");
    let s = single_of(&w.r, "onep", &out, Some("assets")).unwrap();
    let text = read(&out);
    let refs = w.id("$refs");
    let warn = |t: &str| {
        format!(
            "warning: area/guide: {refs}: the ref to {} is not in this file; it is written as its label",
            w.id(t)
        )
    };
    // the org profile's own warning first, then one per ref written as its label, in page order:
    // a block in a collapsed section, the collapsed section, a block of a doc outside the file
    assert_eq!(s.warnings.len(), 4, "{:?}", s.warnings);
    assert!(
        s.warnings[0].starts_with("Org profile has 1 references"),
        "{:?}",
        s.warnings
    );
    assert_eq!(s.warnings[1..], [warn("$hid"), warn("$fold"), warn("$op")]);
    for w in &s.warnings[1..] {
        assert_eq!(w.matches("warning: ").count(), 1, "one prefix: {w}");
    }
    // the nearest heading at or above the target, a repeated heading, Unicode letters and
    // punctuation; an own-page fragment; a link to a page outside the file is its words
    assert!(text.contains(
        "See [Deep words.](#über-größe-20), [the umlaut part](#über-größe-20), [Install](#install), \
         [second](#install-1), folded, Folded, other and [own](#install) and Other page and \
         [ext](https://example.com/a)."
    ));
    // every slug names a heading of the file
    let headings: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix('#'))
        .map(|l| l.trim_start_matches('#').trim().to_string())
        .collect();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let known: Vec<String> = headings[1..]
        .iter()
        .map(|h| {
            let base = print::single_md::slug(h);
            let n = seen.entry(base.clone()).or_default();
            *n += 1;
            if *n == 1 {
                base
            } else {
                format!("{base}-{}", *n - 1)
            }
        })
        .collect();
    for cap in regex::Regex::new(r"\]\(#([^)]+)\)")
        .unwrap()
        .captures_iter(&text)
    {
        assert!(
            known.contains(&cap[1].to_string()),
            "{} in {known:?}",
            &cap[1]
        );
    }
    // the source print of the doc, where the other doc is printed: its ref is still not in
    // this file
    let (_d, mut r) = tiny();
    let ids = put(
        &mut r,
        "x",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$a", "fields": {"title": "C'est l'été — 1/2"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "To {{$s}}."}
        ]),
    );
    // a fragment of the doc's own page that names a block leads to its heading
    put(
        &mut r,
        "y",
        json!([{"op": "update", "id": ids["$p"].to_string(),
            "body": format!("To {{{{{}}}}} and [here](#{}).", ids["$s"], ids["$p"])}]),
    );
    let a = dir.path().join("a.md");
    let s = single(&r, &a, None).unwrap();
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    assert_eq!(
        read(&a),
        "# A\n\n## C'est l'été — 1/2\n\nTo [C'est l'été — 1/2](#cest-lété--12) and [here](#cest-lété--12).\n"
    );
}

// --- 6. assets ----------------------------------------------------------------------------------

#[test]
fn rfr3_single_assets_use_sha256_of_exact_bytes() {
    let w = world();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("README.md");
    let s = single_of(&w.r, "onep", &out, Some("img/all")).unwrap();
    // light and dark, and the second figure of the light bytes shares its file
    assert_eq!(s.assets, 3);
    let files = tree(dir.path());
    let want: BTreeMap<String, Option<Vec<u8>>> = [
        ("README.md".to_string(), Some(std::fs::read(&out).unwrap())),
        ("img/".to_string(), None),
        ("img/all/".to_string(), None),
        (
            format!("img/all/{}.png", sha(&w.light)),
            Some(w.light.clone()),
        ),
        (
            format!("img/all/{}.png", sha(&w.dark)),
            Some(w.dark.clone()),
        ),
        (
            format!("img/all/{}.png", sha(&w.plain)),
            Some(w.plain.clone()),
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(files, want);
    let text = read(&out);
    assert_eq!(
        text.matches(&format!("img/all/{}.png", sha(&w.light)))
            .count(),
        2
    );
    // the remote image stays a link and no file of it is written
    assert!(text.contains("![Build](https://img.example.com/badge.svg)"));
    // the extension is the tree's for the media type, `bin` for one it does not know
    let tree_out = dir.path().join("tree");
    print::export_profile(&w.r.read(), "canon", "onep", &tree_out, Format::Md, None).unwrap();
    let tree_assets: Vec<String> = tree(&tree_out)
        .into_keys()
        .filter(|k| k.contains("assets/") && !k.ends_with('/'))
        .collect();
    assert!(
        !tree_assets.is_empty() && tree_assets.iter().all(|k| k.ends_with(".png")),
        "{tree_assets:?}"
    );
    assert!(asset_name(b"x", "image/png").ends_with(".png"));
    assert!(asset_name(b"x", "image/jpeg").ends_with(".jpg"));
    assert!(asset_name(b"x", "image/svg+xml").ends_with(".svg"));
    assert_eq!(
        asset_name(b"x", "image/x-new"),
        format!("{}.bin", sha(b"x"))
    );
    // a doc with no figure writes no folder, even when one is named
    let (_d, mut r) = tiny();
    put(
        &mut r,
        "n",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "n", "title": "N", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "No figure ![remote](https://img.example.com/r.png)."}
        ]),
    );
    let n = tempfile::tempdir().unwrap();
    let s = single(&r, &n.path().join("n.md"), Some("assets")).unwrap();
    assert_eq!(s.assets, 0);
    assert_eq!(tree(n.path()).keys().collect::<Vec<_>>(), ["n.md"]);
}

// --- 7. refusals --------------------------------------------------------------------------------

#[test]
fn rfr3_single_refusals_preserve_both_destinations() {
    let w = world();
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    // what is there already: a file, a folder with a file, an empty folder, a folder and a link
    // to it, and a file in a folder
    std::fs::write(base.join("held.md"), b"mine").unwrap();
    std::fs::create_dir(base.join("full")).unwrap();
    std::fs::write(base.join("full").join("x"), b"x").unwrap();
    std::fs::create_dir(base.join("empty")).unwrap();
    std::fs::create_dir(base.join("real")).unwrap();
    rdy_qa::platform::symlink(&base.join("real"), &base.join("lnk")).unwrap();
    std::fs::write(base.join("afile"), b"f").unwrap();
    let before = tree(base);
    let out = base.join("README.md");
    let check = |assets: Option<&str>, code: &str, message: &str, fix: &str| {
        let e = single_of(&w.r, "onep", &out, assets).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (code, message, fix),
            "{assets:?}"
        );
        assert_eq!(
            tree(base),
            before,
            "{assets:?}: both destinations as they were"
        );
    };
    // an occupied output, a file or a folder
    for held in ["held.md", "full"] {
        let e = single_of(&w.r, "onep", &base.join(held), Some("assets")).unwrap_err();
        assert_eq!(
            (e.code, e.message.clone(), e.fix.as_str()),
            (
                E_ALREADY_EXISTS,
                format!(
                    "{} exists; rdy export never overwrites a file",
                    base.join(held).display()
                ),
                "pick another path, or remove the file"
            )
        );
        assert_eq!(tree(base), before);
    }
    // invalid assets paths
    let invalid = "pass a relative folder path with no .. part, such as assets";
    let abs = base.join("abs").display().to_string();
    for given in [
        "",
        abs.as_str(),
        "/x",
        ".",
        "./",
        "..",
        "../x",
        "a/../b",
        "README.md",
        "README.md/x",
        "lnk",
        "lnk/x",
        "afile/x",
    ] {
        check(
            Some(given),
            E_BAD_INPUT,
            &format!("--assets {given} must be a folder below the folder of --out"),
            invalid,
        );
    }
    check(
        Some("afile"),
        E_BAD_INPUT,
        "afile is a file: --assets names a folder",
        "pass a folder path such as assets",
    );
    check(
        Some("full"),
        E_BAD_INPUT,
        "full is not empty",
        "choose an empty directory (or one that does not exist yet) so no stale files mix into the export",
    );
    // a figure needs a folder
    check(
        None,
        E_BAD_INPUT,
        &format!("figure {} needs --assets <dir>", w.id("$fp")),
        "pass --assets with a folder beside the file",
    );
    // a public profile whose check blocks writes nothing
    let e = single_of(&w.r, "pubp", &out, Some("assets")).unwrap_err();
    assert_eq!(e.code, E_PUBLICATION_REFERENCE);
    assert_eq!(tree(base), before);
    // a folder inside the project's .rdy directory, by a path below the project
    let root = w.r.read().root().to_path_buf();
    let e = single_of(&w.r, "onep", &root.join("README.md"), Some(".rdy/x")).unwrap_err();
    assert_eq!(
        e.message,
        "--assets .rdy/x must be a folder below the folder of --out"
    );
    assert!(!root.join("README.md").exists() && !root.join(".rdy/x").exists());
    // a failed image or final write removes what the call made and keeps the rest
    for at in [0, 1, 2, usize::MAX] {
        for folder in ["new/deeper", "empty"] {
            let e = with_failed_write_for_tests(at, || {
                single_of(&w.r, "onep", &out, Some(folder)).unwrap_err()
            });
            assert!(
                e.message.contains("an injected write failure"),
                "{at} {folder}: {e}"
            );
            assert_eq!(
                tree(base),
                before,
                "{at} {folder}: only what the call made is gone"
            );
        }
    }
    // the pre-existing empty folder is used, and kept
    let s = single_of(&w.r, "onep", &out, Some("empty")).unwrap();
    assert_eq!(s.assets, 3);
    assert_eq!(std::fs::read_dir(base.join("empty")).unwrap().count(), 3);
    assert_eq!(std::fs::read(base.join("held.md")).unwrap(), b"mine");
}

// --- 8. determinism -----------------------------------------------------------------------------

#[test]
fn rfr3_single_is_deterministic_and_no_network_is_used() {
    let w = world();
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // no network: the writer's crate has no client of any network protocol, and the remote
    // image stays its link with no file fetched
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../rdy-render/Cargo.toml"),
    )
    .unwrap();
    for client in [
        "reqwest",
        "ureq",
        "hyper",
        "curl",
        "isahc",
        "attohttpc",
        "surf",
    ] {
        assert!(!manifest.contains(client), "{client}");
    }
    let one = single_of(&w.r, "onep", &a.path().join("README.md"), Some("assets")).unwrap();
    let two = single_of(&w.r, "onep", &b.path().join("README.md"), Some("assets")).unwrap();
    assert_eq!(tree(a.path()), tree(b.path()));
    assert_eq!(
        (one.bytes, one.assets, &one.warnings),
        (two.bytes, two.assets, &two.warnings)
    );
    // nested lists and generated pictures: the same bytes again
    let text = read(&a.path().join("README.md"));
    assert!(text.contains("<picture>\n<source media=\"(prefers-color-scheme: dark)\""));
    let files: Vec<String> = tree(a.path()).into_keys().collect();
    assert!(
        !files.iter().any(|f| f.contains("badge")),
        "no remote file: {files:?}"
    );
    let (_d, mut r) = tiny();
    put(
        &mut r,
        "n",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "n", "title": "N", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "- a\n  - b\n    1. c\n\n       d\n- e"}
        ]),
    );
    let (x, y) = (a.path().join("n1.md"), a.path().join("n2.md"));
    single(&r, &x, None).unwrap();
    single(&r, &y, None).unwrap();
    assert_eq!(std::fs::read(&x).unwrap(), std::fs::read(&y).unwrap());
    assert_eq!(read(&x), "# N\n\n- a\n  - b\n    1. c\n\n       d\n- e\n");
}

// --- 9. the old layouts -------------------------------------------------------------------------

#[test]
fn rfr3_old_layouts_keep_their_contract() {
    let w = world();
    let dir = tempfile::tempdir().unwrap();
    for (layout, name) in [
        (None, "plain"),
        (Some(Layout::Mkdocs), "mkdocs"),
        (Some(Layout::Techdocs), "techdocs"),
    ] {
        let old = dir.path().join(format!("{name}-old"));
        let new = dir.path().join(format!("{name}-new"));
        let a =
            print::export_profile(&w.r.read(), "canon", "onep", &old, Format::Md, layout).unwrap();
        let b = print::export_profile_with(
            &w.r.read(),
            "canon",
            "onep",
            &new,
            Format::Md,
            &ExportOptions {
                layout: layout.map(ExportLayout::Tree),
                assets: None,
            },
        )
        .unwrap();
        assert_eq!(tree(&old), tree(&new), "{name}");
        // the result keys are the old ones: no layout
        let v = serde_json::to_value(&a).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "assets", "bytes", "docs", "folders", "format", "out", "pages", "warnings"
            ],
            "{name}"
        );
        assert_eq!(b.bytes, a.bytes);
        // tree paths and anchors stay; no reference mark reaches a tree file
        let files = tree(&old);
        let doc = files
            .iter()
            .find(|(k, _)| k.ends_with("guide.md"))
            .and_then(|(_, v)| v.clone())
            .map(|v| String::from_utf8(v).unwrap())
            .unwrap();
        assert!(
            doc.contains("rdy_id:") && doc.contains("<a id=\""),
            "{name}: {doc}"
        );
        assert!(!doc.contains("data-rdy-ref"), "{name}");
        assert!(
            doc.contains("Shell"),
            "{name}: the code label stays in the tree"
        );
    }
    // the file formats keep their result keys too
    for (format, name) in [(Format::Pdf, "o.pdf"), (Format::Html, "o.html")] {
        let s = print::export_profile(
            &w.r.read(),
            "canon",
            "onep",
            &dir.path().join(name),
            format,
            None,
        )
        .unwrap();
        let v = serde_json::to_value(&s).unwrap();
        assert!(v.get("layout").is_none(), "{name}");
    }
    // the site and its Copy payload carry no reference mark
    let site = tempfile::tempdir().unwrap();
    rdy_render::render_site(&w.r.read(), &Scope::Canon, site.path()).unwrap();
    for (path, bytes) in tree(site.path()) {
        if let Some(b) = bytes {
            assert!(
                !String::from_utf8_lossy(&b).contains("data-rdy-ref"),
                "{path}"
            );
        }
    }
}
