//! rdx15: the end-to-end canary (D-CC90E: removal lands only with a person's approval; D-3M81W:
//! the site and the print show `Content removed.`; D-KQ4CA: chat first, the CLI with parity;
//! D-EPK1Z: retained issue text stays readable).
//!
//! Two served projects hold the same content: a term whose body and name are selected canaries,
//! a decision whose ruling is one (it shows in a sibling's trail), a figure whose picture the
//! removal selects, an open view that revises the term, and a review, a tension and an answer
//! that quote the term's alias whole (the retained audit quote canary). In the first project an
//! agent dry-runs, records and proposes the removal over MCP, reads it pending and relays the
//! person's bound answer; in the second a person does the same through the CLI and approves as
//! a human. Both adapters give the shared op's JSON, errors and formatter text. No byte leaves
//! before the approval; after it, no new output holds a selected canary or the picture: MCP and
//! CLI reads and searches, the static site and its assets, search index, llms files, review and
//! trail, and the print as Markdown, HTML and PDF (its text decoded through each font's
//! ToUnicode map, its images read from their objects). Outputs are built from the static state;
//! live cache races belong to Task 14.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
use rdy_engine::redaction::{RedactResult, report_text};
use rdy_engine::{DecisionInput, Rdy, ReadState, Scope, Session};
use rdy_model::redaction::{CONTENT_REMOVED, ImpactKind, ImpactRelation, Redaction};
use rdy_model::{BlockId, Hash, Review, Role, SemanticTension};
use rdy_qa::daemon::Served;
use rdy_qa::every_kind::verdict;
use rdy_qa::fixtures::{land, write, writer};
use rdy_qa::mcp::{McpClient, Reply};
use rdy_qa::tensions::{ag, put, upd};
use rdy_render::print::fonts::Fonts;
use rdy_render::print::{self};
use rdy_render::{Mode, build_site};
use rdy_store::kinds;
use regex::Regex;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

/// The selected canaries: the term's body and name, and the decision's ruling. Latin script:
/// the PDF prints them with its own glyphs.
const TERM_BODY: &str = "Kestrel body canary";
const TERM_NAME: &str = "Kestrel name canary";
const RULING: &str = "Kestrel ruling canary";
const SELECTED: [&str; 3] = [TERM_BODY, TERM_NAME, RULING];
/// The retained audit quote canary: the term's alias, which a review finding, a tension's
/// advice and an answer's note quote whole. Those records stay readable (D-EPK1Z).
const AUDIT: &str = "Osprey audit quote";
/// The picture the removal selects, by its size in pixels.
const PICTURE: (u32, u32) = (11, 6);
const PERSON: &str = "human:qa";

// --- the project --------------------------------------------------------------------------------

/// Runs `f` on the project with the daemon stopped (it holds the store while it runs).
fn with_rdy<T>(s: &Served, f: impl FnOnce(&mut Rdy) -> T) -> T {
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let out = f(&mut r);
    r.close().unwrap();
    out
}

/// The ids of a project's content.
#[derive(Clone)]
struct Ids {
    doc: BlockId,
    term: BlockId,
    decision: BlockId,
    blob: Hash,
}

impl Ids {
    /// The removal's selectors: every version of the term and of the decision, and the picture.
    fn targets(&self) -> Vec<String> {
        vec![
            self.term.to_string(),
            self.decision.to_string(),
            format!("blob:{}", self.blob),
        ]
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for b in data {
        c ^= u32::from(*b);
        for _ in 0..8 {
            c = if c & 1 == 1 {
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
    for x in data {
        a = (a + u32::from(*x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// An 8-bit RGB PNG of `w` × `h` in colour `rgb`, its scanlines unfiltered and stored.
fn rgb_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let line: Vec<u8> = std::iter::once(0)
        .chain(rgb.iter().copied().cycle().take(3 * w as usize))
        .collect();
    let raw = line.repeat(h as usize);
    let mut z = vec![0x78, 0x01, 1];
    let len = raw.len() as u16;
    z.extend_from_slice(&len.to_le_bytes());
    z.extend_from_slice(&(!len).to_le_bytes());
    z.extend_from_slice(&raw);
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = vec![];
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &z);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

fn relaying_agent() -> Session {
    Session::agent("agent:relay".into(), Some("chat".into()), Role::Writer).unwrap()
}

/// The content, written into the served project `s` with its daemon stopped:
///
/// - the person lands doc `plan`: section `Part` holds the term (body and name selected, alias
///   the audit quote), a readable sibling, a figure of the picture, and a decision that affects
///   the sibling (its ruling, set before it lands, is selected; the change messages hold no
///   canary); tagged v1.0.0;
/// - agent `ed` revises the term in its open view and proposes it; the person snoozes that
///   proposal through a relay whose note quotes the alias;
/// - a reviewer's verdict on the term and the sibling quotes the alias in its finding and its
///   advice.
fn content(s: &Served) -> Ids {
    with_rdy(s, |r| {
        let blob = r
            .store_mut()
            .put_blob(&rgb_png(PICTURE.0, PICTURE.1, [200, 30, 30]))
            .unwrap();
        let w = write(
            r,
            "work",
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
                {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": TERM_NAME, "aliases": [AUDIT]}, "body": format!("{TERM_BODY}.")},
                {"op": "create", "type": "prose", "id": "$b", "parent": "$s", "body": "A readable sibling."},
                {"op": "create", "type": "figure", "id": "$f", "parent": "$s", "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": "Plain figure alt"}}
            ]),
        )
        .unwrap();
        let id = |k: &str| BlockId::parse(&w.created[k]).unwrap();
        let (doc, section, term, sibling) = (id("$d"), id("$s"), id("$t"), id("$b"));
        let d = r
            .decide(
                &writer(),
                "work",
                DecisionInput {
                    parent: section.to_string(),
                    statement: "A placeholder ruling".into(),
                    affects: vec![sibling.to_string()],
                    ..Default::default()
                },
            )
            .unwrap();
        let decision = BlockId::parse(&d.created["$decision"]).unwrap();
        write(
            r,
            "work",
            json!([{"op": "update", "id": decision.to_string(), "fields": {"statement": RULING}}]),
        )
        .unwrap();
        land(r, "work").unwrap();
        r.tag(&rdy_qa::fixtures::approver(), "v1.0.0", None, None)
            .unwrap();
        // the open view that revises the term, snoozed by the person with a note quoting the alias
        put(r, "ed", upd(&term, &format!("{TERM_BODY}, revised.")));
        let p = r
            .propose(&ag("ed"), "ed", "ed", Some("Revises the term."))
            .unwrap();
        let q = r
            .read()
            .pending(&PendingQuery {
                person: PERSON.into(),
                session_ref: None,
                max_tokens: CARD_MAX_TOKENS,
            })
            .unwrap();
        let item = q
            .items
            .iter()
            .find(|i| i.kind == ItemKind::Approve && i.r#ref == p.r#ref)
            .expect("the revision is queued")
            .clone();
        r.answer(
            &relaying_agent(),
            PERSON,
            AnswerRequest {
                r#ref: item.r#ref,
                option: "later".into(),
                card_hash: Some(item.card_hash),
                note: Some(format!("Ask after {AUDIT} is checked.")),
                relay: RelayInput {
                    evidence: Some(format!("Later: {AUDIT}.")),
                    ..RelayInput::default()
                },
                brief: None,
            },
        )
        .unwrap();
        verdict(
            r,
            &term,
            &sibling,
            Hash([15; 32]),
            &format!("The sibling restates {AUDIT}."),
            &format!("Check {AUDIT} first."),
        );
        Ids {
            doc,
            term,
            decision,
            blob,
        }
    })
}

// --- outputs ----------------------------------------------------------------------------------

/// Every output of one project, by name: the static site of canon, of the open view (a review
/// render) and of the tag; the print as Markdown files, single HTML and PDF; the MCP read and
/// search; the CLI show and search.
type Outputs = BTreeMap<String, Vec<u8>>;

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// The static outputs of `st`: each site and the print.
fn static_outputs(st: &ReadState) -> Outputs {
    let mut out = Outputs::new();
    for scope in ["canon", "view:ed", "tag:v1.0.0"] {
        let sc = st.resolve_scope(scope).unwrap();
        for (path, bytes) in build_site(st, &sc, Mode::Static).unwrap().files {
            out.insert(format!("{scope}: {path}"), bytes);
        }
    }
    let p = print::build(st, &Scope::Canon).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let md = dir.path().join("md");
    print::md::write_markdown(&p, &md, print::Layout::Plain).unwrap();
    for f in walk(&md) {
        let name = format!("print: md/{}", f.strip_prefix(&md).unwrap().display());
        out.insert(name, std::fs::read(&f).unwrap());
    }
    out.insert("print: single.html".into(), print::single::single_html(&p));
    out.insert(
        "print: print.pdf".into(),
        print::pdf::pdf_bytes(&p, &Fonts::load()).0,
    );
    out
}

/// The MCP and CLI reads of the project: the doc and a search for the canaries' word.
fn adapter_outputs(s: &Served, ids: &Ids) -> Outputs {
    let mut out = Outputs::new();
    let mut c = McpClient::start_proxy(s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:reader", "run": "rd", "role": "reader"}),
    )
    .ok();
    for scope in ["canon", "view:ed"] {
        let read = c
            .call(
                "rdy_read",
                json!({"id": ids.doc.to_string(), "scope": scope, "format": "markdown"}),
            )
            .ok();
        out.insert(
            format!("mcp: rdy_read {scope}"),
            read.to_string().into_bytes(),
        );
    }
    let found = c.call("rdy_search", json!({"query": "Kestrel"})).ok();
    out.insert("mcp: rdy_search".into(), found.to_string().into_bytes());
    close(c);
    let term = ids.term.to_string();
    let decision = ids.decision.to_string();
    for (name, args) in [
        ("cli: show", vec!["show", term.as_str()]),
        ("cli: show decision", vec!["show", decision.as_str()]),
        ("cli: search", vec!["search", "Kestrel"]),
    ] {
        let o = s.rdy(&args);
        assert!(
            o.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        out.insert(name.into(), o.stdout);
    }
    out
}

fn close(mut c: McpClient) {
    c.close_stdin();
    let (code, _) = c.wait_exit(Duration::from_secs(30));
    assert_eq!(code, Some(0), "the server exits cleanly");
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The RFC 4648 base64 text `b64` decoded (padding ends it); `None` for a byte outside the
/// alphabet.
fn unbase64(b64: &str) -> Option<Vec<u8>> {
    let mut out = vec![];
    let (mut acc, mut n) = (0u32, 0u32);
    for c in b64.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((acc >> n) as u8);
            acc &= (1 << n) - 1;
        }
    }
    Some(out)
}

/// The decoded payload of each base64 data URI in `t` (single-file HTML embeds its pictures and
/// fonts this way, so their bytes never show raw).
fn data_uri_payloads(t: &str) -> Vec<Vec<u8>> {
    Regex::new(r"data:[^;,\s]*;base64,([A-Za-z0-9+/]*=*)")
        .unwrap()
        .captures_iter(t)
        .filter_map(|c| unbase64(&c[1]))
        .collect()
}

/// Every output and every selected canary it holds; a PDF is read through its text and its
/// images (`PICTURE` stands for the picture), an asset by its bytes, and a text output also by
/// the decoded payloads of its base64 data URIs.
fn holdings(outputs: &Outputs, picture: &[u8]) -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    for (name, bytes) in outputs {
        let mut found = BTreeSet::new();
        let t = if name.ends_with(".pdf") {
            if pdf_images(bytes).contains(&(i64::from(PICTURE.0), i64::from(PICTURE.1))) {
                found.insert("picture".to_string());
            }
            pdf_text(bytes)
        } else {
            text(bytes)
        };
        for c in SELECTED {
            if t.contains(c) {
                found.insert(c.to_string());
            }
        }
        if find(bytes, picture, 0).is_some()
            || data_uri_payloads(&t)
                .iter()
                .any(|p| find(p, picture, 0).is_some())
        {
            found.insert("picture".to_string());
        }
        out.insert(name.clone(), found);
    }
    out
}

/// Whether any segment file of the project at `dir` holds `needle`.
fn on_disk(dir: &Path, needle: &[u8]) -> bool {
    std::fs::read_dir(dir.join(".rdy/store/segments"))
        .unwrap()
        .any(|e| find(&std::fs::read(e.unwrap().path()).unwrap(), needle, 0).is_some())
}

/// The frames on disk whose payload holds `needle`, as (logical kind, hash).
fn frames_holding(dir: &Path, needle: &[u8]) -> Vec<(u8, Hash)> {
    let mut out = vec![];
    for (name, start, end, kind, hash) in frames_on_disk(dir) {
        let bytes = std::fs::read(dir.join(".rdy/store/segments").join(&name)).unwrap();
        if find(&bytes[start..end], needle, 0).is_some() {
            out.push((rdy_store::codec::logical_kind(kind), hash));
        }
    }
    out
}

/// The ASSESSMENT records a request's report lists as `erasable: false`, by hash (ruling
/// task-10 Amendment 1, item 4: a selected canary may stay in exactly these records' bytes).
fn kept_assessments(report: &Value) -> BTreeSet<Hash> {
    report["inside"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "assessment" && r["erasable"] == false)
        .map(|r| r["record"].as_str().unwrap().parse().unwrap())
        .collect()
}

/// Each frame of each segment file: (file, payload start, payload end, kind, hash).
fn frames_on_disk(dir: &Path) -> Vec<(String, usize, usize, u8, Hash)> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir.join(".rdy/store/segments")).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".log") {
            continue;
        }
        let bytes = std::fs::read(&p).unwrap();
        let mut at = 0usize;
        while at + 37 <= bytes.len() {
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            let kind = bytes[at + 4];
            let hash = Hash(bytes[at + 5..at + 37].try_into().unwrap());
            out.push((name.clone(), at + 37, at + 37 + len, kind, hash));
            at += 37 + len;
        }
    }
    out
}

/// Before the approval: every occurrence of a selected canary on disk lies in a frame the
/// request's manifest drops (a selected version, or a change that embeds one), and the picture
/// lies in a dropped chunk.
fn canaries_only_in_selected_payloads(dir: &Path, req: &Redaction, picture: &[u8]) {
    let spans: BTreeSet<(u8, Hash)> = req
        .manifest
        .spans
        .iter()
        .map(|s| (s.kind, s.hash))
        .collect();
    let frames = frames_on_disk(dir);
    for (name, start, end, kind, hash) in &frames {
        let bytes = std::fs::read(dir.join(".rdy/store/segments").join(name)).unwrap();
        let payload = &bytes[*start..*end];
        for needle in SELECTED.iter().map(|c| c.as_bytes()).chain([picture]) {
            if find(payload, needle, 0).is_some() {
                assert!(
                    spans.contains(&(rdy_store::codec::logical_kind(*kind), *hash)),
                    "a selected canary is in frame {kind} {hash}, which the removal keeps"
                );
            }
        }
    }
}

// --- PDF reading ------------------------------------------------------------------------------

/// One object of a PDF: its dictionary's text and its stream's bytes.
struct Obj {
    dict: String,
    stream: Option<Vec<u8>>,
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| from + i)
}

/// Every object of `pdf`, by number, read where its `N 0 obj` line starts.
fn objects(pdf: &[u8]) -> BTreeMap<u32, Obj> {
    let length = Regex::new(r"/Length (\d+)").unwrap();
    let mut out = BTreeMap::new();
    let mut at = 0;
    while let Some(s) = find(pdf, b" 0 obj\n", at) {
        let mut n = s;
        while n > 0 && pdf[n - 1].is_ascii_digit() {
            n -= 1;
        }
        let line_start = n == 0 || pdf[n - 1] == b'\n';
        let start = s + 7;
        let Some(endobj) = find(pdf, b"\nendobj", start) else {
            break;
        };
        at = endobj;
        if !line_start || n == s {
            continue;
        }
        let id: u32 = std::str::from_utf8(&pdf[n..s]).unwrap().parse().unwrap();
        let obj = match find(pdf, b">>\nstream\n", start) {
            Some(st) if st < endobj => {
                let dict = String::from_utf8_lossy(&pdf[start..st + 2]).into_owned();
                let len: usize = length.captures(&dict).expect("a length")[1]
                    .parse()
                    .unwrap();
                let data = st + 10;
                at = data + len;
                Obj {
                    dict,
                    stream: Some(pdf[data..data + len].to_vec()),
                }
            }
            _ => Obj {
                dict: String::from_utf8_lossy(&pdf[start..endobj]).into_owned(),
                stream: None,
            },
        };
        out.insert(id, obj);
    }
    out
}

/// LSB-first bits of a DEFLATE stream.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
}

impl Bits<'_> {
    fn bits(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for i in 0..n {
            let b = (self.data[self.pos] >> self.bit) & 1;
            v |= u32::from(b) << i;
            self.bit += 1;
            if self.bit == 8 {
                self.bit = 0;
                self.pos += 1;
            }
        }
        v
    }
    fn align(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.pos += 1;
        }
    }
}

/// A canonical Huffman code: how many codes of each length, and the symbols in code order.
struct Huff {
    count: [u16; 16],
    symbol: Vec<u16>,
}

fn huff(lengths: &[u8]) -> Huff {
    let mut count = [0u16; 16];
    for l in lengths {
        count[usize::from(*l)] += 1;
    }
    count[0] = 0;
    let mut offs = [0u16; 16];
    for i in 1..16 {
        offs[i] = offs[i - 1] + count[i - 1];
    }
    let mut symbol = vec![0u16; lengths.len()];
    for (s, l) in lengths.iter().enumerate() {
        if *l != 0 {
            symbol[usize::from(offs[usize::from(*l)])] = s as u16;
            offs[usize::from(*l)] += 1;
        }
    }
    Huff { count, symbol }
}

fn decode(b: &mut Bits, h: &Huff) -> u16 {
    let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
    for len in 1..16 {
        code |= b.bits(1) as i32;
        let count = i32::from(h.count[len]);
        if code - count < first {
            return h.symbol[(index + (code - first)) as usize];
        }
        index += count;
        first += count;
        first <<= 1;
        code <<= 1;
    }
    panic!("an invalid Huffman code")
}

const LBASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEXT: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DEXT: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn codes(b: &mut Bits, lit: &Huff, dist: &Huff, out: &mut Vec<u8>) {
    loop {
        let sym = decode(b, lit);
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return,
            _ => {
                let i = usize::from(sym - 257);
                let len = usize::from(LBASE[i]) + b.bits(LEXT[i]) as usize;
                let d = usize::from(decode(b, dist));
                let back = usize::from(DBASE[d]) + b.bits(DEXT[d]) as usize;
                let from = out.len() - back;
                for k in 0..len {
                    out.push(out[from + k]);
                }
            }
        }
    }
}

/// A zlib stream inflated (RFC 1950, RFC 1951): stored, fixed and dynamic blocks. rdy-qa
/// carries no inflate dependency, and a PDF's streams are small.
fn inflate_zlib(data: &[u8]) -> Vec<u8> {
    let mut b = Bits {
        data: &data[2..],
        pos: 0,
        bit: 0,
    };
    let mut out = vec![];
    loop {
        let last = b.bits(1);
        match b.bits(2) {
            0 => {
                b.align();
                let len = usize::from(u16::from_le_bytes([b.data[b.pos], b.data[b.pos + 1]]));
                b.pos += 4;
                out.extend_from_slice(&b.data[b.pos..b.pos + len]);
                b.pos += len;
            }
            1 => {
                let mut l = vec![8u8; 288];
                l[144..256].fill(9);
                l[256..280].fill(7);
                codes(&mut b, &huff(&l), &huff(&[5u8; 30]), &mut out);
            }
            2 => {
                let nlen = b.bits(5) as usize + 257;
                let ndist = b.bits(5) as usize + 1;
                let ncode = b.bits(4) as usize + 4;
                const ORDER: [usize; 19] = [
                    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                ];
                let mut cl = [0u8; 19];
                for o in ORDER.iter().take(ncode) {
                    cl[*o] = b.bits(3) as u8;
                }
                let lencode = huff(&cl);
                let mut lengths = vec![];
                while lengths.len() < nlen + ndist {
                    let sym = decode(&mut b, &lencode);
                    match sym {
                        0..=15 => lengths.push(sym as u8),
                        16 => {
                            let prev = *lengths.last().expect("a length to repeat");
                            let n = 3 + b.bits(2);
                            lengths.extend(std::iter::repeat_n(prev, n as usize));
                        }
                        17 => {
                            let n = 3 + b.bits(3);
                            lengths.extend(std::iter::repeat_n(0, n as usize));
                        }
                        _ => {
                            let n = 11 + b.bits(7);
                            lengths.extend(std::iter::repeat_n(0, n as usize));
                        }
                    }
                }
                let lit = huff(&lengths[..nlen]);
                let dist = huff(&lengths[nlen..]);
                codes(&mut b, &lit, &dist, &mut out);
            }
            _ => panic!("an invalid DEFLATE block"),
        }
        if last == 1 {
            return out;
        }
    }
}

fn inflate(o: &Obj) -> Vec<u8> {
    let data = o.stream.as_ref().expect("a stream");
    if o.dict.contains("/Filter /FlateDecode") {
        inflate_zlib(data)
    } else {
        data.clone()
    }
}

fn refs(dict: &str, pattern: &str) -> Vec<(String, u32)> {
    Regex::new(pattern)
        .unwrap()
        .captures_iter(dict)
        .map(|c| (c[1].to_string(), c[2].parse().unwrap()))
        .collect()
}

/// A ToUnicode map's code → text, from its `bfchar` entries.
fn to_unicode(cmap: &[u8]) -> BTreeMap<u16, String> {
    let text = String::from_utf8(cmap.to_vec()).unwrap();
    let mut out = BTreeMap::new();
    let pair = Regex::new(r"<([0-9A-Fa-f]{4})>\s*<([0-9A-Fa-f]+)>").unwrap();
    for block in text.split("beginbfchar").skip(1) {
        let block = &block[..block.find("endbfchar").unwrap()];
        for c in pair.captures_iter(block) {
            let code = u16::from_str_radix(&c[1], 16).unwrap();
            let units: Vec<u16> = c[2]
                .as_bytes()
                .chunks(4)
                .map(|u| u16::from_str_radix(std::str::from_utf8(u).unwrap(), 16).unwrap())
                .collect();
            out.insert(code, String::from_utf16(&units).unwrap());
        }
    }
    out
}

/// A content stream's strings and operators, in order.
enum Tok {
    Str(Vec<u8>),
    Word(String),
}

fn tokens(c: &[u8]) -> Vec<Tok> {
    let mut out = vec![];
    let mut i = 0;
    while i < c.len() {
        let b = c[i];
        if b.is_ascii_whitespace() {
            i += 1;
        } else if b == b'<' && c.get(i + 1) != Some(&b'<') {
            let e = find(c, b">", i).unwrap();
            let hex: Vec<u8> = c[i + 1..e]
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            out.push(Tok::Str(
                hex.chunks(2)
                    .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
                    .collect(),
            ));
            i = e + 1;
        } else if b == b'(' {
            let mut depth = 1;
            let mut s = vec![];
            i += 1;
            while depth > 0 {
                let b = c[i];
                i += 1;
                match b {
                    b'\\' => {
                        let e = c[i];
                        i += 1;
                        match e {
                            b'n' => s.push(b'\n'),
                            b'r' => s.push(b'\r'),
                            b't' => s.push(b'\t'),
                            b'b' => s.push(8),
                            b'f' => s.push(12),
                            b'0'..=b'7' => {
                                let mut v = u32::from(e - b'0');
                                for _ in 0..2 {
                                    if (b'0'..=b'7').contains(&c[i]) {
                                        v = v * 8 + u32::from(c[i] - b'0');
                                        i += 1;
                                    }
                                }
                                s.push(v as u8);
                            }
                            other => s.push(other),
                        }
                    }
                    b'(' => {
                        depth += 1;
                        s.push(b);
                    }
                    b')' => {
                        depth -= 1;
                        if depth > 0 {
                            s.push(b);
                        }
                    }
                    _ => s.push(b),
                }
            }
            out.push(Tok::Str(s));
        } else {
            let s = i;
            i += 1;
            while i < c.len() && !c[i].is_ascii_whitespace() && !b"/[]<>()".contains(&c[i]) {
                i += 1;
            }
            out.push(Tok::Word(String::from_utf8_lossy(&c[s..i]).into_owned()));
        }
    }
    out
}

/// The text each page of `pdf` draws, decoded through each font's ToUnicode map (an embedded
/// font) or as WinAnsi bytes (Courier, the code face), one line per `Tj`.
fn pdf_text(pdf: &[u8]) -> String {
    let objs = objects(pdf);
    let mut out = String::new();
    let is_page = Regex::new(r"/Type /Page\b").unwrap();
    for page in objs.values().filter(|o| is_page.is_match(&o.dict)) {
        let fonts: BTreeMap<String, u32> =
            refs(&page.dict, r"/(F\d+) (\d+) 0 R").into_iter().collect();
        let content = refs(&page.dict, r"/(Contents) (\d+) 0 R");
        let content = inflate(&objs[&content[0].1]);
        let toks = tokens(&content);
        let mut font = String::new();
        for (k, t) in toks.iter().enumerate() {
            let Tok::Word(op) = t else { continue };
            match op.as_str() {
                "Tf" => {
                    if let Some(Tok::Word(n)) = toks.get(k.wrapping_sub(2)) {
                        font = n.trim_start_matches('/').to_string();
                    }
                }
                "Tj" => {
                    let Some(Tok::Str(bytes)) = toks.get(k.wrapping_sub(1)) else {
                        continue;
                    };
                    let f = &objs[&fonts[&font]].dict;
                    if f.contains("/Subtype /Type0") {
                        let tu = refs(f, r"/(ToUnicode) (\d+) 0 R");
                        let map = to_unicode(&inflate(&objs[&tu[0].1]));
                        for g in bytes.chunks(2) {
                            let g = u16::from_be_bytes([g[0], g[1]]);
                            out.push_str(map.get(&g).expect("every glyph is in ToUnicode"));
                        }
                    } else {
                        out.extend(bytes.iter().map(|b| char::from(*b)));
                    }
                    out.push('\n');
                }
                _ => {}
            }
        }
    }
    out
}

/// Each image object's `(width, height)`.
fn pdf_images(pdf: &[u8]) -> BTreeSet<(i64, i64)> {
    let num = |d: &str, key: &str| -> i64 {
        Regex::new(&format!(r"/{key} (\d+)"))
            .unwrap()
            .captures(d)
            .map(|c| c[1].parse().unwrap())
            .unwrap()
    };
    objects(pdf)
        .values()
        .filter(|o| o.dict.contains("/Subtype /Image"))
        .map(|o| (num(&o.dict, "Width"), num(&o.dict, "Height")))
        .collect()
}

// --- the adapters -----------------------------------------------------------------------------

/// The `RedactResult` inside a redact reply (the mutation envelope adds its own keys).
fn result_of(v: &Value) -> RedactResult {
    serde_json::from_value(json!({
        "dry_run": v["dry_run"], "reason_class": v["reason_class"], "reason": v["reason"],
        "change": v["change"], "redaction": v["redaction"], "report": v["report"],
        "warnings": v["warnings"],
    }))
    .unwrap_or_else(|e| panic!("{e}"))
}

/// The CLI args of a redact request for `targets`.
fn cli_redact<'a>(targets: &'a [String], extra: &[&'a str]) -> Vec<&'a str> {
    let mut a = vec!["--json", "redact"];
    a.extend(targets.iter().map(String::as_str));
    a.extend(["--reason-class", "secret", "--reason", "a leaked code"]);
    a.extend_from_slice(extra);
    a
}

fn mcp_redact(targets: &[String], dry_run: bool) -> Value {
    json!({"targets": targets, "reason_class": "secret", "reason": "a leaked code", "dry_run": dry_run})
}

/// The error a failed `--json` command printed on stderr: its code, message and fix.
fn cli_error(o: &std::process::Output) -> (String, String, String) {
    assert!(!o.status.success(), "the command succeeded");
    let err = String::from_utf8_lossy(&o.stderr);
    let v: Value = serde_json::from_str(err.trim().lines().last().unwrap_or("")).unwrap();
    let f = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    (f("code"), f("message"), f("fix"))
}

fn mcp_error(r: Reply) -> (String, String, String) {
    let Reply::ToolError(e) = r else {
        panic!("expected a tool error")
    };
    let f = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    (f("code"), f("message"), f("fix"))
}

/// The facts both adapters must show alike for one request, whatever its project's hashes: the
/// report's counts and kinds of rows, the warnings, the class and reason.
fn shape(v: &Value) -> Value {
    let rep = &v["report"];
    let n = |k: &str| rep[k].as_array().map_or(0, Vec::len);
    let rows: BTreeSet<String> = rep["inside"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| format!("{} {}", r["kind"], r["relation"]))
        .collect();
    let quotes: BTreeSet<String> = rep["quotes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| format!("{} {} {}", q["kind"], q["quote"], q["erasable"]))
        .collect();
    json!({
        "keys": v.as_object().unwrap().keys().collect::<Vec<_>>(),
        "versions": n("versions"), "blobs": n("blobs"), "shared_blobs": n("shared_blobs"),
        "drop_frames": rep["drop_frames"], "drop_bytes": rep["drop_bytes"],
        "rows": rows, "quotes": quotes, "outside": rep["outside"], "assessment": rep["assessment"],
        "warnings": v["warnings"], "reason_class": v["reason_class"], "reason": v["reason"],
    })
}

/// The removal card facts of a pending item, without its project's hashes.
fn card_shape(item: &Value) -> Value {
    let mut r = item["removal"].clone();
    for x in r.as_array_mut().unwrap() {
        x.as_object_mut().unwrap().remove("request");
        x.as_object_mut().unwrap().remove("report");
    }
    r
}

/// The person's pending item for proposal version `r#ref` in a `pending` reply.
fn pending_item(v: &Value, r#ref: &str) -> Value {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == r#ref)
        .unwrap_or_else(|| panic!("{ref} is not pending"))
        .clone()
}

#[test]
fn rdx15_no_selected_canary_reaches_a_new_output() {
    let picture = rgb_png(PICTURE.0, PICTURE.1, [200, 30, 30]);
    let mut shapes = vec![];
    let mut cards = vec![];
    let img_tag = Regex::new(r#"<img src="data:image/png;base64,[^"]*"[^>]*>"#).unwrap();
    for adapter in ["mcp", "cli"] {
        let s = Served::new();
        let ids = content(&s);
        let targets = ids.targets();
        // before: each output holds a selected canary or the picture
        let before = with_rdy(&s, |r| static_outputs(&r.read()));
        let mut before_all = before.clone();
        before_all.extend(adapter_outputs(&s, &ids));
        let held = holdings(&before_all, &picture);
        for want in [
            "canon: plan.html",
            "canon: search-index.json",
            "canon: llms-full.txt",
            "canon: glossary.html",
            "view:ed: plan.html",
            "tag:v1.0.0: plan.html",
            "print: single.html",
            "print: print.pdf",
            "mcp: rdy_read canon",
            "mcp: rdy_read view:ed",
            "mcp: rdy_search",
            "cli: show",
            "cli: show decision",
            "cli: search",
        ] {
            assert!(
                held.get(want).is_some_and(|h| !h.is_empty()),
                "{adapter}: before, {want} holds a selected canary: {:?}",
                held.get(want)
            );
        }
        assert!(
            held.iter()
                .any(|(k, h)| k.starts_with("print: md/") && !h.is_empty()),
            "{adapter}: the Markdown print"
        );
        assert!(
            held.iter()
                .any(|(k, h)| k.starts_with("canon: assets/") && h.contains("picture")),
            "{adapter}: the picture is an asset"
        );
        assert!(held["print: print.pdf"].contains("picture"), "{adapter}");
        // the single-file HTML embeds the picture as a base64 data URI, which the detector reads
        assert!(
            held["print: single.html"].contains("picture"),
            "{adapter}: the single HTML's picture"
        );
        // the PDF's text, decoded through its ToUnicode maps, holds the term's body (a raw grep
        // of the compressed file could not see it)
        let pdf = &before["print: print.pdf"];
        assert!(pdf_text(pdf).contains(TERM_BODY), "{adapter}: the PDF text");
        assert!(
            find(pdf, TERM_BODY.as_bytes(), 0).is_none(),
            "{adapter}: not as raw bytes"
        );
        // the trail of the sibling quotes the ruling, and the open view's review shows the term
        let plan = text(&before["canon: plan.html"]);
        assert!(
            plan.contains(&format!("Ruled: {RULING}")),
            "{adapter}: the trail"
        );
        assert!(
            held["view:ed: plan.html"].contains(TERM_BODY),
            "{adapter}: the review"
        );

        // the request, through the adapter, and its approval by the person
        let (rec, item) = if adapter == "mcp" {
            let mut c = McpClient::start_proxy(&s);
            c.call(
                "rdy_session",
                json!({"actor": "agent:a", "run": "r1", "role": "writer"}),
            )
            .ok();
            // the dry run: the tool and the CLI give the one op reply and the shared text
            let dry = c.call("rdy_redact", mcp_redact(&targets, true)).ok();
            let (ok, cli_dry) = s.json(&cli_redact(&targets, &["--dry-run"]));
            assert!(ok, "{cli_dry}");
            assert_eq!(dry, cli_dry, "the adapters give one dry-run reply");
            let want = report_text(&result_of(&dry), 4000).unwrap();
            assert_eq!(dry["text"], want.text.as_str(), "the shared formatter");
            assert!(dry["change"].is_null() && dry["redaction"].is_null());
            // the retained audit quotes are candidates, never targets
            for kind in ["review", "tension", "answer"] {
                assert!(
                    dry["report"]["quotes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|q| q["kind"] == kind
                            && q["quote"] == AUDIT
                            && q["erasable"] == false),
                    "{kind} quotes the alias: {}",
                    dry["report"]["quotes"]
                );
            }
            // one error, alike from both adapters
            let bad = vec!["blob:zz".to_string()];
            let e1 = mcp_error(c.call("rdy_redact", mcp_redact(&bad, true)));
            let e2 = cli_error(&s.rdy(&cli_redact(&bad, &["--dry-run"])));
            assert_eq!(e1, e2, "the adapters give one error");
            assert_eq!(e1.0, "E_REDACTION_TARGET");
            // record, propose, read pending
            let rec = c.call("rdy_redact", mcp_redact(&targets, false)).ok();
            assert_eq!(
                rec["report"], dry["report"],
                "the request binds the dry run"
            );
            let p = c
                .call(
                    "rdy_propose",
                    json!({"title": "Remove the canaries", "summary": "Removes the leaked code."}),
                )
                .ok();
            let pref = p["ref"].as_str().unwrap().to_string();
            let pending = c.call("rdy_pending", json!({})).ok();
            let item = pending_item(&pending, &pref);
            assert!(
                on_disk(s.dir.path(), TERM_BODY.as_bytes()),
                "nothing left yet"
            );
            // an agent alone cannot approve it; the person's bound answer, relayed, does
            let alone = c.call(
                "rdy_answer",
                json!({"ref": pref, "option": "approve", "card_hash": item["card_hash"]}),
            );
            assert!(!alone.is_ok(), "{alone:?}");
            assert!(
                on_disk(s.dir.path(), TERM_BODY.as_bytes()),
                "nothing left yet"
            );
            c.call(
                "rdy_answer",
                json!({"ref": pref, "option": "approve", "card_hash": item["card_hash"],
                       "evidence": "Yes, remove it.", "question": "Remove the leaked code?"}),
            )
            .ok();
            close(c);
            (rec, item)
        } else {
            // the dry run: the CLI's JSON is the op's reply, and its text the shared formatter's
            let (ok, dry) = s.json(&cli_redact(&targets, &["--dry-run"]));
            assert!(ok, "{dry}");
            let mut args = cli_redact(&targets, &["--dry-run"]);
            args.remove(0);
            let o = s.rdy(&args);
            assert!(o.status.success());
            let want = report_text(&result_of(&dry), 4000).unwrap();
            assert_eq!(
                text(&o.stdout),
                format!("{}\n", want.text),
                "the shared text"
            );
            let (ok, rec) = s.json(&cli_redact(&targets, &[]));
            assert!(ok, "{rec}");
            assert_eq!(
                rec["report"], dry["report"],
                "the request binds the dry run"
            );
            let (ok, p) = s.json(&[
                "--json",
                "propose",
                "--title",
                "Remove the canaries",
                "--summary",
                "Removes the leaked code.",
            ]);
            assert!(ok, "{p}");
            let pref = p["ref"].as_str().unwrap().to_string();
            let (ok, pending) = s.json(&["--json", "pending"]);
            assert!(ok, "{pending}");
            let item = pending_item(&pending, &pref);
            // the person reads the same card as text
            let o = s.rdy(&["pending"]);
            assert!(text(&o.stdout).contains(item["card"].as_str().unwrap()));
            // before the approval: the canaries lie only in the payloads the request drops
            let req = with_rdy(&s, |r| {
                r.read()
                    .reader()
                    .redaction_request(&rec["redaction"].as_str().unwrap().parse().unwrap())
                    .unwrap()
            });
            canaries_only_in_selected_payloads(s.dir.path(), &req, &picture);
            assert!(
                on_disk(s.dir.path(), TERM_BODY.as_bytes()),
                "nothing left yet"
            );
            // the person approves as themselves: the card first, then the landing
            let o = s.rdy(&["approve", &pref]);
            let out = text(&o.stdout);
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            assert!(out.contains(item["card"].as_str().unwrap()), "{out}");
            (rec, item)
        };
        assert_eq!(item["removal"][0]["request"], rec["redaction"]);
        assert_eq!(item["removal"][0]["report"], rec["report"]["report_hash"]);
        shapes.push(shape(&rec));
        cards.push(card_shape(&item));

        // after: every selected payload left the disk, except in the bytes of an ASSESSMENT
        // record the report lists as not erasable (D-EPK1Z; ruling task-10 Amendment 1, item
        // 4). The exemption is exactly those records, by hash; the retained audit quotes still
        // read
        let kept = kept_assessments(&rec["report"]);
        for c in SELECTED {
            let held: Vec<(u8, Hash)> = frames_holding(s.dir.path(), c.as_bytes())
                .into_iter()
                .filter(|(k, h)| !(*k == kinds::ASSESSMENT && kept.contains(h)))
                .collect();
            assert!(held.is_empty(), "{adapter}: {c} left in {held:?}");
        }
        assert!(
            on_disk(s.dir.path(), AUDIT.as_bytes()),
            "{adapter}: the audit quotes stay"
        );
        assert!(
            !on_disk(s.dir.path(), &picture),
            "{adapter}: the picture left"
        );
        let after = with_rdy(&s, |r| {
            let st = r.read();
            let req: Hash = rec["redaction"].as_str().unwrap().parse().unwrap();
            assert_eq!(
                st.block(&Scope::Canon, &ids.term).unwrap().body,
                CONTENT_REMOVED
            );
            assert_eq!(
                st.block(&Scope::Canon, &ids.decision).unwrap().body,
                CONTENT_REMOVED
            );
            let record = st.reader().redaction_request(&req).unwrap();
            let mut audit = 0;
            for row in &record.manifest.inside {
                if row.relation != ImpactRelation::Quotes {
                    continue;
                }
                let h: Hash = match row.record.parse() {
                    Ok(h) => h,
                    Err(_) => continue,
                };
                let quoted = match row.kind {
                    ImpactKind::Review => {
                        let v: Review = st.reader().get_typed(kinds::REVIEW, &h).unwrap();
                        serde_json::to_string(&v).unwrap()
                    }
                    ImpactKind::Tension => {
                        let v: SemanticTension = st.reader().get_typed(kinds::TENSION, &h).unwrap();
                        serde_json::to_string(&v).unwrap()
                    }
                    ImpactKind::Answer => {
                        let v: rdy_model::Answer =
                            st.reader().get_typed(kinds::ANSWER, &h).unwrap();
                        serde_json::to_string(&v).unwrap()
                    }
                    _ => continue,
                };
                assert!(
                    quoted.contains(AUDIT),
                    "{adapter}: {:?} still reads",
                    row.kind
                );
                audit += 1;
            }
            assert!(
                audit >= 3,
                "{adapter}: the review, the tension and the answer"
            );
            static_outputs(&st)
        });
        let mut after_all = after.clone();
        after_all.extend(adapter_outputs(&s, &ids));
        let held = holdings(&after_all, &picture);
        let leaks: Vec<(&String, &BTreeSet<String>)> =
            held.iter().filter(|(_, h)| !h.is_empty()).collect();
        assert!(
            leaks.is_empty(),
            "{adapter}: outputs hold a selected canary: {leaks:?}"
        );
        // the detector would fail this test if the picture came back: the before-output's image
        // tag, put back into the single HTML, is found
        let tag = img_tag
            .find(&text(&before["print: single.html"]))
            .expect("the single HTML's image tag")
            .as_str()
            .to_string();
        let mut tampered = after_all.clone();
        tampered
            .get_mut("print: single.html")
            .unwrap()
            .extend_from_slice(tag.as_bytes());
        assert_eq!(
            holdings(&tampered, &picture)["print: single.html"],
            BTreeSet::from(["picture".to_string()]),
            "{adapter}: the picture put back is found"
        );
        // the outputs read the notice in place of the removed blocks; the assets hold no picture
        for name in [
            "canon: plan.html",
            "canon: search-index.json",
            "canon: llms-full.txt",
            "print: single.html",
            "mcp: rdy_read canon",
        ] {
            assert!(
                text(&after_all[name]).contains(CONTENT_REMOVED),
                "{adapter}: {name} reads the notice"
            );
        }
        assert!(pdf_text(&after["print: print.pdf"]).contains(CONTENT_REMOVED));
        assert!(
            !pdf_images(&after["print: print.pdf"])
                .contains(&(i64::from(PICTURE.0), i64::from(PICTURE.1)))
        );
        assert!(
            !after
                .keys()
                .any(|k| k.contains(&format!("assets/{}", ids.blob))),
            "{adapter}: no asset of the picture"
        );
        assert!(
            text(&after["canon: plan.html"]).contains("A readable sibling."),
            "{adapter}: the sibling still reads"
        );
    }
    // the two adapters showed the same request and the same card facts
    assert_eq!(
        shapes[0], shapes[1],
        "the request and report, across adapters"
    );
    assert_eq!(cards[0], cards[1], "the removal card, across adapters");
}

/// Fix round 1, finding 2: the detector reads a picture that an output embeds as a base64 data
/// URI (single-file HTML does), decoding each URI's payload with the RFC 4648 alphabet.
#[test]
fn rdx15_the_canary_detector_reads_data_uri_pictures() {
    for (encoded, raw) in [
        ("Zg==", "f"),
        ("Zm8=", "fo"),
        ("Zm9v", "foo"),
        ("Zm9vYg==", "foob"),
        ("Zm9vYmE=", "fooba"),
        ("Zm9vYmFy", "foobar"),
    ] {
        let html = format!("<p>x</p>\n<img src=\"data:image/png;base64,{encoded}\" alt=\"a\">\n");
        let outputs: Outputs = [("print: single.html".to_string(), html.into_bytes())].into();
        assert_eq!(
            holdings(&outputs, raw.as_bytes())["print: single.html"],
            BTreeSet::from(["picture".to_string()]),
            "{encoded}"
        );
        // the encoded text alone is no match for other bytes
        assert!(holdings(&outputs, b"other bytes")["print: single.html"].is_empty());
    }
}
