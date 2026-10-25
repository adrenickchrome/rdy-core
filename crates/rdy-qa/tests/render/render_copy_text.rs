//! dgm9 (D-BRF4P as amended by D-E8P8J, D-E0YDA, D-GF2CM): Copy puts a block, a section or the
//! whole page on the clipboard as Markdown, as the page shows it. The page carries the Markdown of
//! each block once, in a hidden template; each toolbar names a range of it. A link becomes an
//! absolute URL, resolved against the page's own address when the reader copies, so no id is
//! copied and nothing is requested. The page title of an authored doc or folder gets the block
//! toolbar: Link, Copy ref and Copy.
use rdy_engine::{Rdy, Scope};
use rdy_qa::fixtures::{land, write};
use rdy_qa::golden::{self, html, tiny};
use rdy_qa::site::{unescape, visible_runs};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::BTreeMap;

// --- the fixture --------------------------------------------------------------------------------

/// A PNG of `w` by `h` pixels of one colour.
fn png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn adler32(data: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for &d in data {
            a = (a + d as u32) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let mut line = vec![0u8];
    for _ in 0..w {
        line.extend_from_slice(&rgb);
    }
    let raw = line.repeat(h as usize);
    let mut z = vec![0x78, 0x01, 1];
    z.extend_from_slice(&(raw.len() as u16).to_le_bytes());
    z.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
    z.extend_from_slice(&raw);
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut ihdr = vec![];
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// The copy fixture: folder `Area` (with a description) holding doc `Guide page`, whose lede is
/// one paragraph, then section `First part` (prose with emphasis, inline code and a reference to
/// `Other page`'s section, a list, a table, a code block, nested section `Nested part` with a
/// prose and a drawn diagram, a prose after it, a figure and a term) and section `Second part`;
/// and doc `Other page` with section `Target here`.
struct Fixture {
    _dir: tempfile::TempDir,
    r: Rdy,
    ids: BTreeMap<String, String>,
    blob: String,
}

const GUIDE: &str = "area/guide.html";
const FOLDER: &str = "area/index.html";

fn fixture() -> Fixture {
    let (dir, mut r) = tiny();
    let blob = r
        .store_mut()
        .put_blob(&png(3, 2, [9, 99, 199]))
        .unwrap()
        .to_string();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$other", "fields": {"name": "other", "title": "Other page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$target", "parent": "$other", "fields": {"title": "Target here"}},
            {"op": "create", "type": "prose", "parent": "$target", "body": "Other words."},
            {"op": "create", "type": "folder", "id": "$area", "fields": {"name": "area", "title": "Area", "description": "The area's *own* words."}},
            {"op": "create", "type": "doc", "id": "$guide", "parent": "$area", "fields": {"name": "guide", "title": "Guide page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$lede", "parent": "$guide", "body": "The lede of the guide."},
            {"op": "create", "type": "section", "id": "$s1", "parent": "$guide", "fields": {"title": "First part"}},
            {"op": "create", "type": "prose", "id": "$p1", "parent": "$s1", "body": "Some **bold**, *italic* and `code` words, see {{$target}}."},
            {"op": "create", "type": "prose", "id": "$list", "parent": "$s1", "body": "- one\n- two"},
            {"op": "create", "type": "table", "id": "$table", "parent": "$s1", "fields": {"columns": ["Name", "Count"], "rows": [["a|b", "2"]]}},
            {"op": "create", "type": "code", "id": "$code", "parent": "$s1", "fields": {"lang": "rust"}, "body": "fn main() {}"},
            {"op": "create", "type": "section", "id": "$s2", "parent": "$s1", "fields": {"title": "Nested part"}},
            {"op": "create", "type": "prose", "id": "$inner", "parent": "$s2", "body": "Inside nested."},
            {"op": "create", "type": "diagram", "id": "$dia", "parent": "$s2", "fields": {"lang": "mermaid", "alt": "Two boxes"}, "body": "graph TD\n  A --> B"},
            {"op": "create", "type": "prose", "id": "$after", "parent": "$s1", "body": "After nested."},
            {"op": "create", "type": "figure", "id": "$fig", "parent": "$s1", "fields": {"blob": blob, "media_type": "image/png", "alt": "A small figure"}},
            {"op": "create", "type": "term", "id": "$term", "parent": "$s1", "fields": {"name": "Ledger"}, "body": "The ledger term."},
            {"op": "create", "type": "section", "id": "$s3", "parent": "$guide", "fields": {"title": "Second part"}},
            {"op": "create", "type": "prose", "id": "$p3", "parent": "$s3", "body": "Second words."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let ids = w.created.into_iter().collect();
    Fixture {
        _dir: dir,
        r,
        ids,
        blob,
    }
}

impl Fixture {
    fn id(&self, k: &str) -> &str {
        &self.ids[k]
    }

    /// The static site of `scope`, written to a new directory.
    fn site(&self, scope: &Scope) -> tempfile::TempDir {
        let out = tempfile::tempdir().unwrap();
        rdy_render::render_site(&self.r.read(), scope, out.path()).unwrap();
        out
    }
}

// --- reading the page -------------------------------------------------------------------------

/// The page's one Copy template, as served.
fn template_of(page: &str) -> String {
    let re = Regex::new(r"(?s)<template data-copy-page hidden>(.*?)</template>").unwrap();
    let all: Vec<_> = re.find_iter(page).collect();
    assert_eq!(all.len(), 1, "one Copy template per page");
    all[0].as_str().to_string()
}

/// The page's Copy payload, as the browser reads the template's content.
fn payload_of(page: &str) -> Value {
    let tpl = template_of(page);
    let inner = tpl
        .strip_prefix("<template data-copy-page hidden>")
        .and_then(|t| t.strip_suffix("</template>"))
        .unwrap();
    assert!(!inner.contains('<'), "the payload is escaped text: {inner}");
    serde_json::from_str(&unescape(inner)).unwrap()
}

/// [`payload_of`] as the renderer's own type.
fn payload(page: &str) -> rdy_render::copy::CopyPayload {
    use rdy_render::print::md::CopyPart;
    let v = payload_of(page);
    let blocks = v["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            b.as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    let value = p["value"].as_str().unwrap().to_string();
                    match p["kind"].as_str().unwrap() {
                        "text" => CopyPart::Text(value),
                        "url" => CopyPart::Url(value),
                        k => panic!("a part of kind {k}"),
                    }
                })
                .collect()
        })
        .collect();
    rdy_render::copy::CopyPayload { blocks }
}

/// Every toolbar of the page, as served, in page order.
fn bars_of(page: &str) -> Vec<String> {
    let re = Regex::new(r#"(?s)<span class="tools">.*?</span>"#).unwrap();
    re.find_iter(page).map(|m| m.as_str().to_string()).collect()
}

/// The ids a reader must never be handed: anything shaped like a block id.
fn id_shaped(text: &str) -> Vec<String> {
    let re = Regex::new(r"\b[a-z]{1,3}-[0-9A-HJKMNP-TV-Z]{5}\b").unwrap();
    re.find_iter(text).map(|m| m.as_str().to_string()).collect()
}

// --- the script, in node ----------------------------------------------------------------------

/// A just-enough DOM for `copy.js` (and `code.js`): the served markup parsed into elements, with
/// a `template` element whose parsed content is a DocumentFragment at `content` (its own
/// `textContent` is empty, as in a browser); a clipboard that writes, rejects or is missing; an
/// `execCommand` fallback that copies, refuses or throws; an injected timer; and a count of every
/// request. A scenario is the body of an async function over `h`.
const HARNESS: &str = r##"
const fs = require('fs');
const spec = JSON.parse(fs.readFileSync(0, 'utf8'));
const log = { clip: [], writes: 0, exec: 0, requests: 0, timers: [] };

function decode(s) {
  return s.replace(/&(#x[0-9a-fA-F]+|#[0-9]+|[a-zA-Z]+);/g, (m, e) => {
    if (e[0] === '#') return String.fromCodePoint(e[1] === 'x' || e[1] === 'X' ? parseInt(e.slice(2), 16) : parseInt(e.slice(1), 10));
    return { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'" }[e] || m;
  });
}
function Frag() { this.nodes = []; this.parentNode = null; }
Frag.prototype = {
  get textContent() { return this.nodes.map((n) => (typeof n === 'string' ? n : n.textContent)).join(''); },
};
function El(tag, attrs) {
  this.nodeType = 1; this.tagName = tag.toUpperCase(); this.attrs = attrs || {}; this.nodes = [];
  this.parentNode = null; this.style = {};
}
function matchOne(e, sel) {
  const m = /^([a-zA-Z][a-zA-Z0-9]*)?((?:\.[\w-]+|\[[^\]]+\])*)$/.exec(sel.trim());
  if (!m) throw new Error('the harness does not know the selector ' + sel);
  if (m[1] && e.tagName !== m[1].toUpperCase()) return false;
  const parts = m[2].match(/\.[\w-]+|\[[^\]]+\]/g) || [];
  return parts.every((p) => {
    if (p[0] === '.') return (e.attrs.class || '').split(/\s+/).includes(p.slice(1));
    const a = /^\[([\w-]+)(?:="([^"]*)")?\]$/.exec(p);
    if (!(a[1] in e.attrs)) return false;
    return a[2] === undefined || e.attrs[a[1]] === a[2];
  });
}
El.prototype = {
  get parentElement() { return this.parentNode instanceof El ? this.parentNode : null; },
  get children() { return this.nodes.filter((n) => n instanceof El); },
  get textContent() { return this.nodes.map((n) => (n instanceof El ? n.textContent : n)).join(''); },
  set textContent(v) { this.nodes = [String(v)]; },
  get className() { return this.attrs.class || ''; },
  set className(v) { this.attrs.class = String(v); },
  get nextElementSibling() { const s = this.parentNode ? this.parentNode.children : []; return s[s.indexOf(this) + 1] || null; },
  get previousElementSibling() { const s = this.parentNode ? this.parentNode.children : []; const i = s.indexOf(this); return i > 0 ? s[i - 1] : null; },
  get classList() {
    const self = this;
    const list = () => (self.attrs.class || '').split(/\s+/).filter(Boolean);
    return {
      contains: (c) => list().includes(c),
      add: (c) => { if (!list().includes(c)) self.attrs.class = list().concat([c]).join(' '); },
      remove: (c) => { self.attrs.class = list().filter((x) => x !== c).join(' '); },
      toggle: (c) => { const on = !list().includes(c); if (on) this.add(c); else this.remove(c); return on; },
    };
  },
  getAttribute(n) { return n in this.attrs ? this.attrs[n] : null; },
  hasAttribute(n) { return n in this.attrs; },
  setAttribute(n, v) { this.attrs[n] = String(v); },
  removeAttribute(n) { delete this.attrs[n]; },
  matches(sel) { return sel.split(',').some((s) => matchOne(this, s)); },
  closest(sel) { for (let e = this; e instanceof El; e = e.parentNode) if (e.matches(sel)) return e; return null; },
  querySelectorAll(sel) {
    const out = [];
    const walk = (e) => { for (const c of e.children) { if (c.matches(sel)) out.push(c); walk(c); } };
    walk(this); return out;
  },
  querySelector(sel) { return this.querySelectorAll(sel)[0] || null; },
  appendChild(c) { c.parentNode = this; this.nodes.push(c); return c; },
  removeChild(c) { this.nodes = this.nodes.filter((n) => n !== c); c.parentNode = null; return c; },
  addEventListener() {},
  select() { selected = this; },
};

function parse(html, root) {
  let cur = root;
  const re = /<(\/?)([a-zA-Z][a-zA-Z0-9]*)((?:\s+[\w-]+(?:="[^"]*")?)*)\s*>|([^<]+)/g;
  const VOID = ['br', 'img', 'input', 'hr', 'meta', 'link'];
  let m;
  while ((m = re.exec(html))) {
    if (m[4] !== undefined) { cur.nodes.push(decode(m[4])); continue; }
    if (m[1]) { cur = cur.parentNode; continue; }
    const attrs = {};
    for (const a of m[3].matchAll(/([\w-]+)(?:="([^"]*)")?/g)) attrs[a[1]] = a[2] === undefined ? '' : decode(a[2]);
    const e = new El(m[2], attrs); e.parentNode = cur; cur.nodes.push(e);
    if (m[2].toLowerCase() === 'template') {
      // the parser puts a template's children in its content, a DocumentFragment
      e.content = new Frag(); e.content.parentNode = cur; cur = e.content;
    } else if (!VOID.includes(m[2].toLowerCase())) {
      cur = e;
    }
  }
}

let selected = null, range = null;
const body = new El('body');
for (const chunk of spec.html) parse(chunk, body);
const docL = {};
const fail = function () { log.requests++; throw new Error('no request may be made'); };
globalThis.fetch = fail; globalThis.XMLHttpRequest = fail; globalThis.EventSource = fail; globalThis.WebSocket = fail;
const document = {
  body, activeElement: body,
  addEventListener: (t, f) => { (docL[t] = docL[t] || []).push(f); },
  querySelector: (s) => body.querySelector(s),
  querySelectorAll: (s) => body.querySelectorAll(s),
  createElement: (t) => new El(t),
  createRange: () => ({ selectNodeContents(n) { range = n; } }),
  execCommand: (c) => {
    log.exec++;
    if (spec.exec === 'throw') throw new Error('denied');
    if (spec.exec === 'deny') return false;
    if (selected && 'value' in selected) log.clip.push(selected.value);
    else if (range) log.clip.push(range.textContent);
    return true;
  },
};
const navigator = { sendBeacon: fail };
if (spec.clipboard !== 'none') {
  navigator.clipboard = {
    writeText: (t) => {
      log.writes++;
      if (spec.clipboard === 'reject') return Promise.reject(new Error('denied'));
      log.clip.push(t);
      return Promise.resolve();
    },
  };
}
const location = { href: spec.url };
const pending = [];
const setTimeout = (f, ms) => { log.timers.push(ms); pending.push(f); return pending.length; };
const clearTimeout = (id) => { if (id) pending[id - 1] = null; };
const window = {
  document, navigator, location, fetch: fail, XMLHttpRequest: fail,
  getSelection: () => ({ rangeCount: 0, isCollapsed: true, toString: () => '', removeAllRanges() {}, addRange() {} }),
};
for (const src of spec.scripts) {
  new Function('window', 'document', 'navigator', 'location', 'setTimeout', 'clearTimeout', 'fetch', 'XMLHttpRequest', src)(
    window, document, navigator, location, setTimeout, clearTimeout, fail, fail);
}
const settle = () => new Promise((r) => setImmediate(r));
let selection = null;
window.getSelection = () => selection || { rangeCount: 0, isCollapsed: true, toString: () => '', removeAllRanges() {}, addRange() {} };
const h = {
  out: {}, log,
  buttons: (cls) => body.querySelectorAll('.' + cls),
  q: (sel) => body.querySelector(sel),
  // the reader selects `text` from `anchor` to `focus`: the page hears of the selection change
  select(anchor, focus, text) {
    selection = { rangeCount: 1, isCollapsed: false, anchorNode: anchor, focusNode: focus, toString: () => text };
    (docL.selectionchange || []).forEach((f) => f({ type: 'selectionchange' }));
  },
  async click(btn) {
    const ev = { type: 'click', target: btn, preventDefault() { throw new Error('preventDefault was called'); } };
    (docL.click || []).forEach((f) => f(ev));
    await settle(); await settle();
  },
  // runs the timers set so far, as the clock would
  fire() { pending.splice(0).forEach((f) => f && f()); },
  // where each link slot of the page's payload leads from the page's address
  resolved() {
    const p = JSON.parse(body.querySelector('template[data-copy-page]').content.textContent);
    const m = {};
    for (const b of p.blocks) for (const part of b) if (part.kind === 'url') {
      try { m[part.value] = new URL(part.value, spec.url).href; } catch (e) { m[part.value] = null; }
    }
    return m;
  },
  // clicks `btn` and says what reached the clipboard (null for nothing) and what the button says
  async copy(btn) {
    const n = log.clip.length;
    await h.click(btn);
    return { text: log.clip.length > n ? log.clip[log.clip.length - 1] : null, shown: btn.textContent };
  },
};
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
new AsyncFunction('h', spec.scenario)(h).then(
  () => { h.out.log = log; console.log(JSON.stringify(h.out)); },
  (e) => { console.error((e && e.stack) || e); process.exit(1); },
);
"##;

/// The text of a theme script.
fn script(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../rdy-render/theme/rdy-standard/js/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// Runs `spec` (`html` chunks, `scripts` texts, `url`, `clipboard`, `exec` and the `scenario`)
/// in node and returns what the scenario filled into `h.out`.
fn run(spec: Value) -> Value {
    let mut child = std::process::Command::new("node")
        .args(["-e", HARNESS])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(spec.to_string().as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
}

/// The page's template and toolbars, with `copy.js`, at `url`, with a working clipboard.
fn page_spec(page: &str, url: &str, scenario: &str) -> Value {
    let mut chunks = vec![template_of(page)];
    chunks.extend(bars_of(page));
    json!({"html": chunks, "scripts": [script("copy.js")], "url": url,
        "clipboard": "ok", "exec": "ok", "scenario": scenario})
}

/// Clicks every Copy control of the page in order: each one's range, label and copy.
const COPY_ALL: &str = r#"
    h.out.copies = [];
    for (const b of h.buttons('copy-text')) {
      const c = await h.copy(b);
      h.out.copies.push({ start: b.getAttribute('data-copy-start'), end: b.getAttribute('data-copy-end'),
        label: b.getAttribute('aria-label'), text: c.text, shown: c.shown });
    }
    h.out.resolved = h.resolved();
"#;

/// One copy of [`COPY_ALL`]'s output: its range, label and text.
struct Copied {
    range: rdy_render::copy::CopyRange,
    label: String,
    text: String,
}

/// [`COPY_ALL`]'s copies, each checked against the renderer's own reading of the payload with the
/// same resolved URLs: the Rust and the script results are one.
fn copies(page: &str, v: &Value) -> Vec<Copied> {
    let p = payload(page);
    let resolved: BTreeMap<String, String> = serde_json::from_value(v["resolved"].clone()).unwrap();
    v["copies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let n = |k: &str| c[k].as_str().unwrap().parse::<usize>().unwrap();
            let range = rdy_render::copy::CopyRange {
                start: n("start"),
                end: n("end"),
            };
            let text = c["text"]
                .as_str()
                .unwrap_or_else(|| panic!("{c}"))
                .to_string();
            let rust = p.markdown(range, &mut |u| resolved.get(u).cloned());
            assert_eq!(rust.as_deref(), Some(text.as_str()), "{c}");
            assert_eq!(c["shown"], "Copied", "{c}");
            Copied {
                range,
                label: c["label"].as_str().unwrap().to_string(),
                text,
            }
        })
        .collect()
}

/// The node tests run only where node is installed.
fn node_or_skip(test: &str) -> bool {
    if rdy_qa::skip::node_available() {
        return true;
    }
    rdy_qa::skip::skip(
        test,
        rdy_qa::skip::Why::Prerequisite,
        "node is not installed",
    );
    false
}

// --- the tests --------------------------------------------------------------------------------

/// Each block, section and the whole page copy as the export's Markdown, as the page shows them:
/// headings, emphasis, inline code, a list, a table, a code block in its language, a diagram as
/// its fenced source, a figure as an image and a term. A section holds its heading and every block
/// under it, nested sections included, up to the next heading no deeper than it; the title holds
/// the title, the lede and every block. Links are absolute; no id, anchor line or front matter.
#[test]
fn dgm9_copy_block_section_and_page_markdown() {
    if !node_or_skip("dgm9_copy_block_section_and_page_markdown") {
        return;
    }
    let f = fixture();
    let out = f.site(&Scope::Canon);
    let page = html(out.path(), GUIDE);
    let v = run(page_spec(
        &page,
        "http://h/area/guide.html#first-part",
        COPY_ALL,
    ));
    let all = copies(&page, &v);
    let hash = &f.blob;
    let block = [
        "# Guide page\n".to_string(),
        "The lede of the guide.\n".to_string(),
        "## First part\n".to_string(),
        "Some **bold**, *italic* and `code` words, see [Target here](http://h/other.html#target-here).\n".to_string(),
        "-   one\n-   two\n".to_string(),
        "Table 1\n\n| Name | Count |\n| --- | --- |\n| a\\|b | 2 |\n".to_string(),
        "Rust\n\n```rust\nfn main() {}\n```\n".to_string(),
        "### Nested part\n".to_string(),
        "Inside nested.\n".to_string(),
        "Two boxes\n\n```mermaid\ngraph TD\n  A --> B\n```\n".to_string(),
        "After nested.\n".to_string(),
        format!("![A small figure](http://h/assets/{hash}.png)\n\nA small figure\n"),
        "**Ledger** The ledger term.\n".to_string(),
        "## Second part\n".to_string(),
        "Second words.\n".to_string(),
    ];
    let joined = |r: std::ops::Range<usize>| block[r].join("\n");
    // the title's toolbar, then one per item in page order
    let want: Vec<(usize, usize, &str)> = vec![
        (0, 15, "page"),
        (2, 13, "section"),
        (3, 4, "block"),
        (4, 5, "block"),
        (5, 6, "table"),
        (6, 7, "code"),
        (7, 10, "section"),
        (8, 9, "block"),
        (9, 10, "diagram"),
        (10, 11, "block"),
        (11, 12, "figure"),
        (12, 13, "term"),
        (13, 15, "section"),
        (14, 15, "block"),
    ];
    let got: Vec<(usize, usize, String)> = all
        .iter()
        .map(|c| (c.range.start, c.range.end, c.label.clone()))
        .collect();
    assert_eq!(
        got,
        want.iter()
            .map(|(s, e, k)| (*s, *e, format!("Copy this {k} as Markdown")))
            .collect::<Vec<_>>()
    );
    for c in &all {
        assert_eq!(c.text, joined(c.range.start..c.range.end), "{:?}", c.range);
    }
    // the whole page, and the nested section's boundary
    assert!(
        all[6]
            .text
            .ends_with("```mermaid\ngraph TD\n  A --> B\n```\n")
    );
    assert!(!all[6].text.contains("After nested."));
    assert!(all[1].text.contains("After nested.") && !all[1].text.contains("Second part"));
    let whole = &all[0].text;
    for never in ["<a id=", "---\n", "rdy_id", "data-", "Copy ref", "Link\n"] {
        assert!(!whole.contains(never), "{never}: {whole}");
    }
    assert_eq!(id_shaped(whole), Vec::<String>::new(), "{whole}");
    assert!(!whole.contains(f.id("$target")), "{whole}");

    // a folder's title copies its title and description
    let folder = html(out.path(), FOLDER);
    let v = run(page_spec(&folder, "http://h/area/index.html", COPY_ALL));
    let all = copies(&folder, &v);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].label, "Copy this page as Markdown");
    assert_eq!(all[0].text, "# Area\n\nThe area's *own* words.\n");
}

/// A doc's and a folder's title get the block toolbar, one per heading: Link (the page's URL with
/// no fragment), Copy ref (the title's block, in its grammar) and Copy (the whole page). Every
/// item has one toolbar of Link, Copy ref and Copy; a section has one, not two. A generated page
/// invents no id, Copy ref or Copy.
#[test]
fn dgm9_title_and_section_have_one_toolbar() {
    let f = fixture();
    let out = f.site(&Scope::Canon);
    let page = html(out.path(), GUIDE);
    let guide = f.id("$guide");
    let head_at = page.find("<header class=\"page-head").unwrap();
    let head = &page[head_at..head_at + page[head_at..].find("</header>").unwrap()];
    assert!(
        head.starts_with(&format!(
            "<header class=\"page-head block block-page\" data-rdy-block=\"{guide}\" data-ref-head=\""
        )),
        "{head}"
    );
    let ref_head = unescape(
        Regex::new(r#"data-ref-head="([^"]*)""#)
            .unwrap()
            .captures(head)
            .unwrap()[1]
            .as_ref(),
    );
    let ref_line = unescape(
        Regex::new(r#" data-ref="([^"]*)""#)
            .unwrap()
            .captures(head)
            .unwrap()[1]
            .as_ref(),
    );
    assert_eq!(ref_head, format!("[{guide}] Guide page"));
    assert_eq!(ref_line, format!("[{guide}] Guide page: \"Guide page\""));
    assert_eq!(
        bars_of(head),
        vec![
            "<span class=\"tools\"><button type=\"button\" class=\"tool copy-link\" data-href=\"\" \
             aria-label=\"Copy link to this page\">Link</button><button type=\"button\" \
             class=\"tool copy-ref\" aria-label=\"Copy a reference to this page for an agent \
             chat\">Copy ref</button><button type=\"button\" class=\"tool copy-text\" \
             data-copy-start=\"0\" data-copy-end=\"15\" aria-label=\"Copy this page as \
             Markdown\">Copy</button></span>"
                .to_string()
        ]
    );
    // every item: one toolbar, Link, Copy ref and Copy in that order
    let blocks: Vec<&str> = page.split("<div class=\"block ").skip(1).collect();
    assert_eq!(blocks.len(), 13);
    let tool = Regex::new(r#"class="tool ([a-z-]+)""#).unwrap();
    for b in &blocks {
        let b = &b[..b.find("\n").unwrap_or(b.len())];
        let bars = bars_of(b);
        assert_eq!(bars.len(), 1, "one toolbar: {b}");
        let classes: Vec<&str> = tool
            .captures_iter(&bars[0])
            .map(|c| c.get(1).unwrap().as_str())
            .collect();
        assert_eq!(classes, ["copy-link", "copy-ref", "copy-text"], "{b}");
    }
    assert_eq!(bars_of(&page).len(), 14);
    assert_eq!(page.matches("class=\"tool copy-text\"").count(), 14);

    // a folder's title
    let folder = html(out.path(), FOLDER);
    let area = f.id("$area");
    assert!(
        folder.contains(&format!(
            "<div class=\"block block-page\" data-rdy-block=\"{area}\" data-ref-head=\"[{area}] Area\""
        )),
        "{folder}"
    );
    let bars = bars_of(&folder);
    assert_eq!(bars.len(), 1, "{folder}");
    assert!(bars[0].contains("data-href=\"\" aria-label=\"Copy link to this page\""));
    assert!(bars[0].contains("data-copy-start=\"0\" data-copy-end=\"2\""));

    // generated pages: no title toolbar, no Copy, no payload
    for p in ["index.html", "glossary.html"] {
        let h = html(out.path(), p);
        assert!(
            !h.contains("copy-text") && !h.contains("data-copy-page"),
            "{p}"
        );
        assert!(!h.contains("block-page"), "{p}");
    }

    if !node_or_skip("dgm9_title_and_section_have_one_toolbar") {
        return;
    }
    // Link copies the page's address without its fragment; Copy ref the title's reference
    let mut spec = page_spec(
        &page,
        "http://h/area/guide.html#first-part",
        r#"
        const bar = h.buttons('tools')[0];
        h.out.link = (await h.copy(bar.querySelector('.copy-link'))).text;
        "#,
    );
    // the toolbar inside its block, as served
    spec["html"] = json!([template_of(&page), head]);
    let v = run(spec);
    assert_eq!(v["link"], "http://h/area/guide.html");
}

/// Final review M2: the page title's header is a block, and Cite quotes a selection in it as the
/// page's words. The eyebrow above the title names the folder, not the page, so a selection in
/// it, or one that starts or ends there, offers no Cite; a selection in the title still does.
#[test]
fn dgm9_cite_skips_the_eyebrow() {
    let f = fixture();
    let out = f.site(&Scope::Canon);
    let page = html(out.path(), GUIDE);
    let head_at = page.find("<header class=\"page-head").unwrap();
    let head = &page[head_at..head_at + page[head_at..].find("</header>").unwrap()];
    assert!(head.contains("<p class=\"eyebrow\">"), "{head}");
    if !node_or_skip("dgm9_cite_skips_the_eyebrow") {
        return;
    }
    let mut spec = page_spec(
        &page,
        "http://h/area/guide.html",
        r#"
        const eyebrow = h.q('.eyebrow'), title = h.q('h1');
        h.select(eyebrow, eyebrow, eyebrow.textContent);
        h.out.eyebrow = h.buttons('cite').length;
        h.select(eyebrow, title, eyebrow.textContent + ' Guide');
        h.out.into_title = h.buttons('cite').length;
        h.select(title, eyebrow, 'Guide ' + eyebrow.textContent);
        h.out.from_title = h.buttons('cite').length;
        h.select(title, title, 'Guide page');
        h.out.title = h.buttons('cite').length;
        h.select(eyebrow, eyebrow, eyebrow.textContent);
        h.out.again = h.buttons('cite').length;
        "#,
    );
    spec["html"] = json!([template_of(&page), head]);
    let v = run(spec);
    assert_eq!(
        (
            &v["eyebrow"],
            &v["into_title"],
            &v["from_title"],
            &v["title"],
            &v["again"]
        ),
        (&json!(0), &json!(0), &json!(0), &json!(1), &json!(0)),
        "{v}"
    );
}

/// A link is the absolute URL of its page and the anchor the page gives the target, resolved
/// against the page's own address with its scope prefix; a frozen page's copy leads to the frozen
/// pages and its Copy ref keeps the version pin, but no copy holds an id. The URL escape is the
/// export's, in Rust and in the script alike; a URL that does not resolve, or a range the payload
/// does not hold, copies nothing.
#[test]
fn dgm9_copy_links_are_absolute_and_frozen() {
    let mut f = fixture();
    f.r.tag(&rdy_qa::fixtures::approver(), "v1.0.0", None, None)
        .unwrap();
    let v1: rdy_model::Hash = f.r.read().versions().unwrap()[0].landing.parse().unwrap();
    let out = f.site(&Scope::At(v1));
    let page = html(out.path(), GUIDE);
    let guide = f.id("$guide");
    assert!(
        page.contains(&format!("data-ref-head=\"[{guide}@v1.0.0] Guide page\"")),
        "the title's Copy ref keeps the pin"
    );
    if !node_or_skip("dgm9_copy_links_are_absolute_and_frozen") {
        return;
    }
    let base = "https://h/~tag/v1.0.0/area/guide.html";
    let v = run(page_spec(&page, base, COPY_ALL));
    let all = copies(&page, &v);
    let whole = &all[0].text;
    assert!(
        whole.contains("[Target here](https://h/~tag/v1.0.0/other.html#target-here)"),
        "{whole}"
    );
    assert!(
        whole.contains(&format!(
            "![A small figure](https://h/~tag/v1.0.0/assets/{}.png)",
            f.blob
        )),
        "{whole}"
    );
    assert!(
        !whole.contains("@v1.0.0") && id_shaped(whole).is_empty(),
        "{whole}"
    );

    // the escape of a link target, on hostile targets: the script and the renderer agree
    use rdy_render::copy::{CopyPayload, CopyRange, link_target};
    use rdy_render::print::md::CopyPart;
    let slots = [
        "a b(c)<d>.html#x y",
        "t\tab\nnl\r.html#q&copy;",
        "https://example.invalid/w(1)?a=1&b=2&#123;",
        "mailto:a@example.invalid",
        "../x%20y.html#%28kept%29",
        "../../../../up.html",
    ];
    let one = CopyPayload {
        blocks: slots
            .iter()
            .map(|s| {
                vec![
                    CopyPart::Text("[x](".into()),
                    CopyPart::Url((*s).into()),
                    CopyPart::Text(")\n".into()),
                ]
            })
            .chain([vec![
                CopyPart::Text("[bad](".into()),
                CopyPart::Url("http://[".into()),
                CopyPart::Text(")\n".into()),
            ]])
            .collect(),
    };
    let tpl = format!(
        "<template data-copy-page hidden>{}</template>",
        rdy_render::escape(&one.json())
    );
    let mut buttons = String::from("<span class=\"tools\">");
    for (s, e) in [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 4),
        (4, 5),
        (5, 6),
        (6, 7),
        (0, 6),
    ] {
        buttons.push_str(&format!(
            "<button type=\"button\" class=\"tool copy-text\" data-copy-start=\"{s}\" data-copy-end=\"{e}\">Copy</button>"
        ));
    }
    // ranges the payload does not hold
    for (s, e) in [
        ("2", "1"),
        ("3", "3"),
        ("0", "8"),
        ("-1", "1"),
        ("0", "1.5"),
        ("0x0", "1"),
        ("", "1"),
        ("01", "2"),
    ] {
        buttons.push_str(&format!(
            "<button type=\"button\" class=\"tool copy-text\" data-copy-start=\"{s}\" data-copy-end=\"{e}\">Copy</button>"
        ));
    }
    buttons.push_str("<button type=\"button\" class=\"tool copy-text\">Copy</button></span>");
    for base in ["http://h/dir/p.html", "file:///tmp/s/dir/p.html"] {
        let v = run(
            json!({"html": [&tpl, &buttons], "scripts": [script("copy.js")], "url": base,
            "clipboard": "ok", "exec": "ok", "scenario": r#"
            h.out.copies = [];
            for (const b of h.buttons('copy-text')) h.out.copies.push(await h.copy(b));
            h.out.resolved = h.resolved();
            "#}),
        );
        let resolved: BTreeMap<String, Option<String>> =
            serde_json::from_value(v["resolved"].clone()).unwrap();
        let mut resolve = |u: &str| resolved.get(u).cloned().flatten();
        let got = v["copies"].as_array().unwrap();
        assert_eq!(got.len(), 17);
        for (i, (s, e)) in [
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 4),
            (4, 5),
            (5, 6),
            (6, 7),
            (0, 6),
        ]
        .into_iter()
        .enumerate()
        {
            let rust = one.markdown(CopyRange { start: s, end: e }, &mut resolve);
            assert_eq!(got[i]["text"].as_str(), rust.as_deref(), "{base} {s}..{e}");
            let shown = if rust.is_some() {
                "Copied"
            } else {
                "Not copied"
            };
            assert_eq!(got[i]["shown"], shown, "{base} {s}..{e}");
        }
        for c in &got[8..] {
            assert_eq!(c["text"], Value::Null, "{base}: {c}");
            assert_eq!(c["shown"], "Not copied", "{base}: {c}");
        }
        assert_eq!(v["log"]["requests"], 0);
        let at = |i: usize| got[i]["text"].as_str().unwrap().to_string();
        if base.starts_with("http:") {
            assert_eq!(at(0), "[x](http://h/dir/a%20b%28c%29%3Cd%3E.html#x%20y)\n");
            assert_eq!(at(1), "[x](http://h/dir/tabnl.html#q&amp;copy;)\n");
            assert_eq!(
                at(2),
                "[x](https://example.invalid/w%281%29?a=1&b=2&amp;#123;)\n"
            );
            assert_eq!(at(3), "[x](mailto:a@example.invalid)\n");
            assert_eq!(at(4), "[x](http://h/x%20y.html#%28kept%29)\n");
            assert_eq!(at(5), "[x](http://h/up.html)\n");
        } else {
            assert_eq!(at(4), "[x](file:///tmp/s/x%20y.html#%28kept%29)\n");
        }
        assert_eq!(
            got[6]["text"],
            Value::Null,
            "an unresolvable URL copies nothing"
        );
        assert_eq!(
            got[7]["text"].as_str().unwrap(),
            (0..6).map(at).collect::<Vec<_>>().join("\n")
        );
    }
    // the Rust escape, on the same resolved targets
    assert_eq!(
        link_target("http://h/dir/a%20b(c)%3Cd%3E.html#x%20y"),
        "http://h/dir/a%20b%28c%29%3Cd%3E.html#x%20y"
    );
    assert_eq!(
        link_target("http://h/dir/tabnl.html#q&copy;"),
        "http://h/dir/tabnl.html#q&amp;copy;"
    );
}

/// From a static site opened from disk, Copy works with the page alone: the payload is in the
/// page, every link leads to a file of the site, and nothing is requested.
#[test]
fn dgm9_copy_works_from_file_without_requests() {
    let f = fixture();
    let out = f.site(&Scope::Canon);
    let page = html(out.path(), GUIDE);
    // nothing to fetch and nothing to run: no payload URL, script, handler or JSON element
    for never in [
        "data-copy-url",
        "onclick",
        "application/json",
        "<script type",
    ] {
        assert!(!page.contains(never), "{never}");
    }
    let copy = script("copy.js");
    for never in [
        "fetch(",
        "XMLHttpRequest",
        "sendBeacon",
        "EventSource",
        "WebSocket",
        "import(",
        "eval(",
        "new Function",
    ] {
        assert!(!copy.contains(never), "copy.js: {never}");
    }
    if !node_or_skip("dgm9_copy_works_from_file_without_requests") {
        return;
    }
    let root = format!("file://{}", out.path().canonicalize().unwrap().display());
    let url = format!("{root}/{GUIDE}");
    let v = run(page_spec(&page, &url, COPY_ALL));
    let all = copies(&page, &v);
    let whole = &all[0].text;
    assert!(
        whole.contains(&format!("[Target here]({root}/other.html#target-here)")),
        "{whole}"
    );
    assert!(
        whole.contains(&format!("![A small figure]({root}/assets/{}.png)", f.blob)),
        "{whole}"
    );
    assert_eq!(v["log"]["requests"], 0);
    assert_eq!(v["log"]["exec"], 0);
}

// the render binary includes the shared redaction fixture from several files
#[allow(clippy::duplicate_mod)]
#[path = "../../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

/// What a reader may not see is not in the payload: a named profile copies only its selection
/// (a reference outside it reads as its words), a removed block copies its notice, and a review
/// page copies the current text alone: no prior text of a word diff, no removed line, no review
/// mark.
#[test]
fn dgm9_copy_profiles_and_removed_blocks_do_not_leak() {
    use rdy_model::redaction::RedactTarget;
    use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
    let (dir, mut r) = tiny();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$private", "fields": {"name": "private", "title": "PRIVCANARY page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$secret", "parent": "$private", "body": "PRIVCANARY words."},
            {"op": "create", "type": "doc", "id": "$public", "fields": {"name": "public", "title": "Public page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$ps", "parent": "$public", "fields": {"title": "Shown part"}},
            {"op": "create", "type": "prose", "parent": "$ps", "body": "Points at {{$secret}} there."},
            {"op": "create", "type": "prose", "id": "$gone", "parent": "$ps", "body": "REDCANARY words."},
            {"op": "create", "type": "prose", "parent": "$ps", "body": "Stays readable."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let id = |k: &str| w.created[k].clone();
    write(
        &mut r,
        "admin",
        json!([{"op": "create", "type": "publication_profile", "id": "$p", "parent": id("$admin"),
            "fields": {"key": "pub", "title": "Public site", "audience": "org", "select": [id("$public")]}}]),
    )
    .unwrap();
    land(&mut r, "admin").unwrap();
    r.close().unwrap();
    let gone = rdy_model::BlockId::parse(&id("$gone")).unwrap();
    authorized_batch_fixture(
        &dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![RedactTarget::AllVersions(gone)],
        )],
        &FixtureOptions::approved(Proof::relayed_human()),
    );
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    let profile = rdy_render::build_profile_site(&st, "canon", "pub").unwrap();
    let source = rdy_render::build_site(&st, &Scope::Canon, rdy_render::Mode::Static).unwrap();
    for (what, files) in [("profile", &profile.files), ("source", &source.files)] {
        let page = String::from_utf8(files["public.html"].clone()).unwrap();
        let p = payload(&page);
        let whole = p
            .markdown(
                rdy_render::copy::CopyRange {
                    start: 0,
                    end: p.blocks.len(),
                },
                &mut |u| Some(format!("http://h/{u}")),
            )
            .unwrap();
        assert!(whole.contains("Content removed.\n"), "{what}: {whole}");
        assert!(whole.contains("Stays readable."), "{what}: {whole}");
        assert!(!whole.contains("REDCANARY"), "{what}: {whole}");
        assert!(id_shaped(&whole).is_empty(), "{what}: {whole}");
        if what == "profile" {
            assert!(!whole.contains("PRIVCANARY"), "{what}: {whole}");
            assert!(!whole.contains("private.html"), "{what}: {whole}");
            assert!(whole.contains("unavailable reference"), "{what}: {whole}");
            // no page of the profile carries what it leaves out
            for (path, bytes) in files.iter() {
                let text = String::from_utf8_lossy(bytes);
                if path.ends_with(".html") {
                    assert!(!text.contains("PRIVCANARY"), "{path}");
                    assert!(!text.contains("Admin page"), "{path}");
                }
            }
        }
        for (path, bytes) in files.iter() {
            assert!(
                !String::from_utf8_lossy(bytes).contains("REDCANARY"),
                "{what}: {path}"
            );
        }
    }
    drop(st);
    drop(r);

    // a review page: the current words, and none of the review's own
    let (_d, _r, _ids, out) = golden::review_site(&Scope::View(golden::REVIEW_VIEW.into()));
    let page = html(out.path(), "intro.html");
    // the page holds the word diffs and the removed line, hidden or folded
    assert!(
        page.contains("<del><span class=\"sr-word\">removed: </span>Old</del>")
            && page.contains("<del><span class=\"sr-word\">removed: </span>start</del>")
            && page.contains("<p>Gone soon.</p>"),
        "{page}"
    );
    let p = payload(&page);
    let whole = p
        .markdown(
            rdy_render::copy::CopyRange {
                start: 0,
                end: p.blocks.len(),
            },
            &mut |u| Some(u.to_string()),
        )
        .unwrap();
    assert!(
        whole.contains("New wording.") && whole.contains("Where to begin."),
        "{whole}"
    );
    for never in [
        "Old",
        "start",
        "removed:",
        "added:",
        "Gone soon.",
        "Removed",
        "Edited",
        "Show diff",
    ] {
        assert!(!whole.contains(never), "{never}: {whole}");
    }
}

/// A code block keeps its own copy control, which copies the code alone; the toolbar's Copy
/// copies the block as the page shows it, fenced in its language.
#[test]
fn dgm9_code_copy_still_copies_only_code() {
    if !node_or_skip("dgm9_code_copy_still_copies_only_code") {
        return;
    }
    let f = fixture();
    let out = f.site(&Scope::Canon);
    let page = html(out.path(), GUIDE);
    let code = f.id("$code");
    let at = page.find(&format!("data-rdy-block=\"{code}\"")).unwrap();
    let start = page[..at].rfind("<div class=\"block").unwrap();
    let block = &page[start..start + page[start..].find("\n").unwrap()];
    let spec = json!({"html": [template_of(&page), block],
        "scripts": [script("copy.js"), script("code.js")], "url": "http://h/area/guide.html",
        "clipboard": "ok", "exec": "ok", "scenario": r#"
        h.out.code = await h.copy(h.buttons('copy-code')[0]);
        h.out.text = await h.copy(h.buttons('copy-text')[0]);
        "#});
    let v = run(spec);
    assert_eq!(v["code"]["text"], "fn main() {}");
    assert_eq!(v["text"]["text"], "Rust\n\n```rust\nfn main() {}\n```\n");
    // each control wrote once
    assert_eq!(v["log"]["writes"], 2, "{v}");
}

/// Copy reuses the toolbar's one clipboard path and its feedback: the clipboard API, else the
/// `execCommand` fallback; "Copied" or "Not copied" for 1500 ms, then the label again, through the
/// one timer a button has. The script has one clipboard implementation.
#[test]
fn dgm9_copy_feedback_reuses_existing_timer() {
    let copy = script("copy.js");
    assert_eq!(copy.matches("function copyText(").count(), 1);
    assert_eq!(copy.matches("function flash(").count(), 1);
    assert_eq!(copy.matches("execCommand").count(), 1);
    assert_eq!(copy.matches(".writeText(").count(), 1);
    assert_eq!(copy.matches("setTimeout(").count(), 1);
    if !node_or_skip("dgm9_copy_feedback_reuses_existing_timer") {
        return;
    }
    let f = fixture();
    let out = f.site(&Scope::Canon);
    let page = html(out.path(), GUIDE);
    let first = "# Guide page\n\nThe lede of the guide.\n";
    for (clipboard, exec, shown, writes, execs) in [
        ("ok", "ok", "Copied", 1, 0),
        ("reject", "ok", "Copied", 1, 1),
        ("none", "ok", "Copied", 0, 1),
        ("none", "deny", "Not copied", 0, 1),
        ("reject", "throw", "Not copied", 1, 1),
    ] {
        let mut spec = page_spec(
            &page,
            "http://h/area/guide.html",
            r#"
            const b = h.buttons('copy-text')[0];
            h.out.first = await h.copy(b);
            h.out.timers = h.log.timers.slice();
            h.fire();
            h.out.after = b.textContent;
            // a second copy, then a third before the clock runs: one timer stands
            await h.copy(b); await h.copy(b);
            h.fire();
            h.out.again = b.textContent;
            "#,
        );
        spec["clipboard"] = json!(clipboard);
        spec["exec"] = json!(exec);
        let v = run(spec);
        let what = format!("{clipboard}/{exec}");
        assert_eq!(v["first"]["shown"], shown, "{what}");
        let copied = (shown == "Copied").then(|| {
            // the page title's range is the whole page
            assert!(
                v["first"]["text"].as_str().unwrap().starts_with(first),
                "{what}"
            );
        });
        if copied.is_none() {
            assert_eq!(v["first"]["text"], Value::Null, "{what}");
        }
        assert_eq!(v["timers"], json!([1500]), "{what}");
        assert_eq!(v["after"], "Copy", "{what}");
        assert_eq!(v["again"], "Copy", "{what}");
        assert_eq!(v["log"]["writes"], json!(writes * 3), "{what}");
        assert_eq!(v["log"]["exec"], json!(execs * 3), "{what}");
        assert_eq!(v["log"]["requests"], 0, "{what}");
    }
}

/// The page holds its Copy payload once: one template, outside the content, each block's
/// Markdown once, and every range as indexes alone. It is no reader's text: not in the visible
/// text, the leak scan, the search index, the llms files or the print. A publication's one page
/// carries the same payload bytes as its full site.
#[test]
fn dgm9_payload_is_stored_once_and_is_not_reader_text() {
    let f = fixture();
    let out = f.site(&Scope::Canon);
    let page = html(out.path(), GUIDE);
    let tpl = template_of(&page);
    println!(
        "dgm9 bytes: {GUIDE} html {} payload {}; {FOLDER} html {} payload {}",
        page.len(),
        tpl.len(),
        html(out.path(), FOLDER).len(),
        template_of(&html(out.path(), FOLDER)).len()
    );
    // outside the article, in no block
    let article_end = page.find("</article>").unwrap();
    assert!(page.find(&tpl).unwrap() > article_end);
    let v = payload_of(&page);
    assert_eq!(
        v.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["blocks"],
        "{v}"
    );
    let json = v.to_string();
    for once in [
        "The lede of the guide.",
        "Inside nested.",
        "After nested.",
        "Second words.",
        "fn main() {}",
        "graph TD",
        "The ledger term.",
    ] {
        assert_eq!(json.matches(once).count(), 1, "{once}: {json}");
    }
    assert_eq!(id_shaped(&json), Vec::<String>::new(), "{json}");
    // a toolbar holds two indexes and no text of the payload
    let copy_button =
        Regex::new(r#"<button type="button" class="tool copy-text" data-copy-start="(\d+)" data-copy-end="(\d+)" aria-label="Copy this [a-z]+ as Markdown">Copy</button>"#)
            .unwrap();
    assert_eq!(copy_button.find_iter(&page).count(), 14);
    assert_eq!(page.matches("copy-text").count(), 14);
    // no reader's text
    let runs = visible_runs(&page);
    assert!(
        !runs
            .iter()
            .any(|r| r.text.contains("\"kind\"") || r.text.contains("blocks")),
        "{runs:?}"
    );
    assert_eq!(
        runs.iter()
            .filter(|r| r.text.contains("Inside nested."))
            .count(),
        1
    );
    assert!(rdy_qa::site::leaks(out.path()).is_empty());
    for file in ["llms.txt", "llms-full.txt", "search-index.json"] {
        let text = std::fs::read_to_string(out.path().join(file)).unwrap();
        for never in [
            "\"kind\":\"text\"",
            "\"kind\":\"url\"",
            "&quot;kind&quot;",
            "data-copy",
            "{\"blocks\"",
        ] {
            assert!(!text.contains(never), "{file}: {never}");
        }
    }
    let md = tempfile::tempdir().unwrap();
    let md_out = md.path().join("md");
    rdy_render::print::export(
        &f.r.read(),
        &Scope::Canon,
        &md_out,
        rdy_render::print::Format::Md,
        None,
    )
    .unwrap();
    let printed = std::fs::read_to_string(md_out.join("area/guide.md")).unwrap();
    assert!(
        !printed.contains("\"kind\"") && !printed.contains("Copy this"),
        "{printed}"
    );

    // a publication: the one page's payload is the full site's
    use rdy_engine::publication::{IMPLICIT_ORG, ReadPublicationSource};
    use rdy_render::render_input::capture_inputs;
    let mut f = f;
    f.r.tag(&rdy_qa::fixtures::approver(), "v1.0.0", None, None)
        .unwrap();
    let landing: rdy_model::Hash = f.r.read().versions().unwrap()[0].landing.parse().unwrap();
    let read = f.r.read();
    let inputs = capture_inputs(&read, &Scope::At(landing)).unwrap();
    let source = ReadPublicationSource::with_settings(
        &read,
        &inputs.pack,
        &inputs.title,
        inputs.description.as_deref(),
    );
    let m = read.materialized(&Scope::At(landing)).unwrap();
    let full =
        rdy_render::build_publication(&source, &m, "v1.0.0", false, IMPLICIT_ORG, &inputs).unwrap();
    for path in [GUIDE, FOLDER] {
        let one = rdy_render::render_publication_page(
            &source,
            &m,
            "v1.0.0",
            false,
            IMPLICIT_ORG,
            &inputs,
            path,
        )
        .unwrap();
        let full_page = String::from_utf8(full.files[path].clone()).unwrap();
        let one_page = String::from_utf8(one.html).unwrap();
        assert_eq!(template_of(&one_page), template_of(&full_page), "{path}");
        assert_eq!(
            template_of(&full_page),
            template_of(&html(out.path(), path)),
            "{path}"
        );
    }
}

/// The browser model: a template's own `textContent` is empty and its parsed text is in
/// `content`. The script reads `content`; a script that read the element's own text would copy
/// nothing.
#[test]
fn dgm9_copy_reads_the_template_content_not_its_own_text() {
    if !node_or_skip("dgm9_copy_reads_the_template_content_not_its_own_text") {
        return;
    }
    let f = fixture();
    let out = f.site(&Scope::Canon);
    let page = html(out.path(), GUIDE);
    let copy = script("copy.js");
    assert_eq!(copy.matches(".content.textContent").count(), 1);
    let scenario = "h.out.first = await h.copy(h.buttons('copy-text')[0]);";
    let v = run(page_spec(&page, "http://h/area/guide.html", scenario));
    assert_eq!(v["first"]["shown"], "Copied");
    let mut spec = page_spec(&page, "http://h/area/guide.html", scenario);
    spec["scripts"] = json!([copy.replace(".content.textContent", ".textContent")]);
    let v = run(spec);
    assert_eq!(
        v["first"]["shown"], "Not copied",
        "the element's own text is empty"
    );
    assert_eq!(v["first"]["text"], Value::Null);
}
