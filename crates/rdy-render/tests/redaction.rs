//! rdx11: the site and the print show removed content as one notice (D-3M81W, D-CC90E, D-X61DT).
//! A removed block, whatever its type, reads `Content removed.` on every page, in the search
//! index, the llms files, the trail, the change pages and every print format. A figure whose blob
//! lost a chunk reads the same notice and its bytes never reach a decoder. A removed folder or doc
//! keeps its children's pages and links under a neutral id path. Tags show only a `redacted`
//! badge: no reason, actor or call to action.
//!
//! The store holds Task 2's raw authorized batch, written by the shared fixture while the project
//! is closed; the outputs are built from the projected state the project reads after it opens.
#[path = "../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::*;
use rdy_model::redaction::{CONTENT_REMOVED, RedactTarget};
use rdy_model::*;
use rdy_render::print::fonts::Fonts;
use rdy_render::print::{self, PBlock, Print};
use rdy_render::{Mode, build_site, menu};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The word every removed text holds (Latin script: the PDF prints it with its own glyphs).
const CANARY: &str = "CANARY";

/// The notice as a page writes it in place of a removed block.
const NOTICE_HTML: &str = "<p class=\"redacted\">Content removed.</p>";

/// The request's reason and the words of its approval: none may reach the site or the print.
const REASON: &str = "a leaked key";

fn session(actor: &str, role: Role) -> Session {
    Session {
        actor: actor.into(),
        run: None,
        role,
        kind: rdy_engine::SessionKind::Human,
    }
}

fn agent() -> Session {
    session("agent:a", Role::Writer)
}

fn founder() -> Session {
    session("human:founder", Role::Approver)
}

/// The one summary a test gives a version that lands a proposed decision (D-DKJS5).
const TEST_SUMMARY: &str = "test summary";

/// Before a landing, the person confirms each exception beside version `p` in their queue.
fn confirm_exceptions(r: &mut Rdy, s: &Session, p: &ProposalInfo) {
    use rdy_engine::answer::{AnswerRequest, relay::RelayInput};
    use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
    let q = r
        .read()
        .pending(&PendingQuery {
            person: s.actor.clone(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending");
    let refs: Vec<String> = q
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(p.r#ref.as_str()))
        .map(|i| i.r#ref.clone())
        .collect();
    for item in refs {
        let req = AnswerRequest {
            r#ref: item.clone(),
            option: "confirm".into(),
            card_hash: None,
            note: None,
            relay: RelayInput::default(),
            brief: None,
        };
        r.answer(s, &s.actor, req)
            .unwrap_or_else(|e| panic!("confirm {item}: {e}"));
    }
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &agent(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy, view: &str) -> String {
    let p = r.propose(&agent(), view, view, Some(TEST_SUMMARY)).unwrap();
    confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap()
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

// --- PNG files a figure shows, written with no compression -------------------------------------

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

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// An 8-bit RGB PNG of `w` × `h` in colour `rgb`, its scanlines unfiltered and stored.
fn rgb_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    // each scanline: filter type 0, then the pixels
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
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

// --- the scenario ------------------------------------------------------------------------------

/// Every output a build writes of one scope or one print, by name.
type Outputs = BTreeMap<String, Vec<u8>>;

/// What the scenario built.
struct World {
    dir: tempfile::TempDir,
    request: Hash,
    /// The folder, the doc and the blocks the request removes.
    folder: BlockId,
    removed_doc: BlockId,
    removed_section: BlockId,
    removed_prose: BlockId,
    removed_term: BlockId,
    removed_figure: BlockId,
    removed_question: BlockId,
    decision: BlockId,
    /// Blocks no request touches.
    plan: BlockId,
    sibling: BlockId,
    links: BlockId,
    kept_doc: BlockId,
    kept_section: BlockId,
    image_figure: BlockId,
    shared_figure: BlockId,
    plain_figure: BlockId,
    /// The image blob the request removes directly (a normal figure shows it).
    image_blob: Hash,
    /// The image blob of the removed figure (no request touches the blob).
    stub_blob: Hash,
    /// The blob that loses a chunk it shares with the removed blob.
    shared_blob: Hash,
    /// The blob no request touches.
    plain_blob: Hash,
    /// Before the removal: each output, by scope and file.
    before: BTreeMap<String, Outputs>,
    before_print: Print,
}

impl World {
    fn open(&self) -> Rdy {
        Rdy::open(self.dir.path()).unwrap()
    }
}

/// The scopes the tests render: canon (it has a summary of the changes since v1.0.0), the open
/// view `edit` (a review render with a change summary and a removed line) and the tag.
const SCOPES: [&str; 3] = ["canon", "view:edit", "tag:v1.0.0"];

/// Every file of the site of `scope`, built in memory.
fn site(st: &ReadState, scope: &str) -> Outputs {
    let scope = st.resolve_scope(scope).unwrap();
    build_site(st, &scope, Mode::Static).unwrap().files
}

/// The print of canon in every format: the Markdown tree's files under `md/`, `single.html` and
/// `print.pdf`.
fn prints(st: &ReadState) -> (Print, Outputs) {
    let p = print::build(st, &Scope::Canon).unwrap();
    let mut out = Outputs::new();
    let dir = tempfile::tempdir().unwrap();
    let md = dir.path().join("md");
    print::md::write_markdown(&p, &md, print::Layout::Plain).unwrap();
    for f in walk(&md) {
        let name = format!("md/{}", f.strip_prefix(&md).unwrap().display());
        out.insert(name, std::fs::read(&f).unwrap());
    }
    out.insert("single.html".into(), print::single::single_html(&p));
    out.insert(
        "print.pdf".into(),
        print::pdf::pdf_bytes(&p, &Fonts::load()).0,
    );
    (p, out)
}

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

/// Every output of the project as it reads now: the site of each scope, and the print.
fn outputs(st: &ReadState) -> (BTreeMap<String, Outputs>, Print) {
    let mut all = BTreeMap::new();
    for s in SCOPES {
        all.insert(s.to_string(), site(st, s));
    }
    let (p, files) = prints(st);
    all.insert("print".into(), files);
    (all, p)
}

/// One project, tagged twice:
///
/// - landing L0 holds an unrelated doc, tagged v0.1.0;
/// - landing L1 holds a folder `vault` (a canary title and description) with a kept doc; a doc
///   `secret` (a canary title) with a kept section; and the doc `plan`: a section with a canary
///   heading holding a canary prose and a readable sibling; a section of links to the removed
///   section (once with a canary display text), the removed term and the kept doc; a question
///   with a canary prompt; a term with a canary name; four figures (an image whose blob the
///   request removes, a removed figure, a download whose blob shares a chunk with a removed blob,
///   a plain image); and a decision with a canary statement that affects the sibling. It is
///   tagged v1.0.0;
/// - landing L2 edits the sibling and the canary prose again (canon counts one landing since
///   v1.0.0);
/// - an open view `edit` rewrites the links and deletes the shared-chunk figure.
///
/// Then one request removes every version of the folder, the doc `secret`, the canary section,
/// the canary prose, the term, the removed figure, the decision and the question, the image
/// blob, and a blob whose only full chunk the download's blob shares.
fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    write(
        &mut r,
        "w0",
        json!([
            {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "other", "title": "Other", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$o", "body": "Unrelated text."}
        ]),
    );
    land(&mut r, "w0");
    r.tag(&founder(), "v0.1.0", None, Some("first".into()))
        .unwrap();

    let image_bytes = rgb_png(7, 5, [200, 30, 30]);
    let stub_bytes = rgb_png(9, 4, [30, 200, 30]);
    let plain_bytes = rgb_png(3, 2, [30, 30, 200]);
    let removed_bytes = vec![7u8; 1 << 20];
    let mut shared_bytes = removed_bytes.clone();
    shared_bytes.extend_from_slice(b"tail");
    let image_blob = r.store_mut().put_blob(&image_bytes).unwrap();
    let stub_blob = r.store_mut().put_blob(&stub_bytes).unwrap();
    let plain_blob = r.store_mut().put_blob(&plain_bytes).unwrap();
    let removed_blob = r.store_mut().put_blob(&removed_bytes).unwrap();
    let shared_blob = r.store_mut().put_blob(&shared_bytes).unwrap();
    let w = write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "vault", "title": "CANARY folder title", "description": "CANARY folder description."}},
            {"op": "create", "type": "doc", "id": "$k", "parent": "$f", "fields": {"name": "kept", "title": "Kept page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$k", "body": "Kept text under the folder."},
            {"op": "create", "type": "doc", "id": "$rd", "fields": {"name": "secret", "title": "CANARY doc title", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$rs", "parent": "$rd", "fields": {"title": "Kept section"}},
            {"op": "create", "type": "prose", "parent": "$rs", "body": "Kept child text."},
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "CANARY heading"}},
            {"op": "create", "type": "prose", "id": "$p1", "parent": "$s", "body": "CANARY body."},
            {"op": "create", "type": "prose", "id": "$p2", "parent": "$s", "body": "Readable sibling."},
            {"op": "create", "type": "section", "id": "$s2", "parent": "$d", "fields": {"title": "Links"}},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s2", "fields": {"name": "CANARY term"}, "body": "A CANARY definition."},
            {"op": "create", "type": "prose", "id": "$p3", "parent": "$s2", "body": "See {{$s}}, {{$t}} and {{$k}}. Also {{$s|CANARY label}}."},
            {"op": "create", "type": "question", "id": "$q", "parent": "$s2", "fields": {"prompt": "CANARY question?"}},
            {"op": "create", "type": "figure", "id": "$fi", "parent": "$s2", "fields": {"blob": image_blob.to_string(), "media_type": "image/png", "alt": "CANARY image alt"}},
            {"op": "create", "type": "figure", "id": "$fs", "parent": "$s2", "fields": {"blob": stub_blob.to_string(), "media_type": "image/png", "alt": "CANARY figure alt"}},
            {"op": "create", "type": "figure", "id": "$fh", "parent": "$s2", "fields": {"blob": shared_blob.to_string(), "media_type": "application/octet-stream", "alt": "CANARY download"}},
            {"op": "create", "type": "figure", "id": "$fp", "parent": "$s2", "fields": {"blob": plain_blob.to_string(), "media_type": "image/png", "alt": "Plain picture"}}
        ]),
    );
    let id = |k: &str| bid(&w.created[k]);
    let (sibling, links) = (id("$p2"), id("$s2"));
    let dec = r
        .decide(
            &agent(),
            "w1",
            DecisionInput {
                parent: links.to_string(),
                statement: "CANARY ruling.".into(),
                options: vec!["CANARY option".into(), "other".into()],
                affects: vec![sibling.to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    let decision = bid(dec.created.values().next().expect("the decision"));
    land(&mut r, "w1");
    r.tag(&founder(), "v1.0.0", None, Some("plan".into()))
        .unwrap();
    write(
        &mut r,
        "w2",
        json!([
            {"op": "update", "id": sibling.to_string(), "body": "Readable sibling, edited."},
            {"op": "update", "id": id("$p1").to_string(), "body": "CANARY body two."}
        ]),
    );
    land(&mut r, "w2");
    write(
        &mut r,
        "edit",
        json!([
            {"op": "update", "id": id("$p3").to_string(), "body": "See {{".to_string() + &id("$s").to_string() + "}} again, and {{" + &id("$t").to_string() + "}}."},
            {"op": "delete", "id": id("$fh").to_string()}
        ]),
    );
    let world_ids = (
        id("$f"),
        id("$rd"),
        id("$s"),
        id("$p1"),
        id("$t"),
        id("$fs"),
        id("$d"),
        id("$k"),
        id("$rs"),
        id("$fi"),
        id("$fh"),
        id("$fp"),
        id("$q"),
    );
    let (before, before_print) = outputs(&r.read());
    r.close().unwrap();
    let fixture = authorized_batch_fixture(
        &dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![
                RedactTarget::AllVersions(world_ids.0.clone()),
                RedactTarget::AllVersions(world_ids.1.clone()),
                RedactTarget::AllVersions(world_ids.2.clone()),
                RedactTarget::AllVersions(world_ids.3.clone()),
                RedactTarget::AllVersions(world_ids.4.clone()),
                RedactTarget::AllVersions(world_ids.5.clone()),
                RedactTarget::AllVersions(decision.clone()),
                RedactTarget::AllVersions(world_ids.12.clone()),
                RedactTarget::Blob(image_blob),
                RedactTarget::Blob(removed_blob),
            ],
        )],
        &FixtureOptions::approved(Proof::relayed_human()),
    );
    let manifest = &fixture.requests[0].draft.request.manifest;
    assert_eq!(
        manifest.shared_blobs,
        vec![shared_blob],
        "the download's blob shares the removed chunk"
    );
    World {
        dir,
        request: fixture.requests[0].request,
        folder: world_ids.0,
        removed_doc: world_ids.1,
        removed_section: world_ids.2,
        removed_prose: world_ids.3,
        removed_term: world_ids.4,
        removed_figure: world_ids.5,
        decision,
        plan: world_ids.6,
        sibling,
        links,
        kept_doc: world_ids.7,
        kept_section: world_ids.8,
        image_figure: world_ids.9,
        shared_figure: world_ids.10,
        plain_figure: world_ids.11,
        removed_question: world_ids.12,
        image_blob,
        stub_blob,
        shared_blob,
        plain_blob,
        before,
        before_print,
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The files of `outputs` whose bytes hold `needle`.
fn holding(outputs: &BTreeMap<String, Outputs>, needle: &str) -> Vec<String> {
    let mut out = vec![];
    for (scope, files) in outputs {
        for (path, bytes) in files {
            if path.ends_with(".pdf") {
                continue;
            }
            if text(bytes).contains(needle) {
                out.push(format!("{scope}: {path}"));
            }
        }
    }
    out
}

fn page(outputs: &BTreeMap<String, Outputs>, scope: &str, path: &str) -> String {
    let files = &outputs[scope];
    text(
        files
            .get(path)
            .unwrap_or_else(|| panic!("{scope} has no {path}: {:?}", files.keys())),
    )
}

#[test]
fn rdx11_all_outputs_use_the_one_removed_notice() {
    let w = world();
    // the canary is everywhere before: each kind of output holds it
    let before = holding(&w.before, CANARY);
    for want in [
        "canon: plan.html",
        "canon: search-index.json",
        "canon: llms.txt",
        "canon: llms-full.txt",
        "canon: changes-since.html",
        "canon: glossary.html",
        "canon: index.html",
        "view:edit: changes.html",
        "view:edit: plan.html",
        "tag:v1.0.0: plan.html",
        "print: single.html",
    ] {
        assert!(before.contains(&want.to_string()), "{want}: {before:?}");
    }
    assert!(
        before.iter().any(|f| f.starts_with("print: md/")),
        "{before:?}"
    );
    // the trail of the sibling quoted the ruling before
    let plan = page(&w.before, "canon", "plan.html");
    assert!(plan.contains("Ruled: CANARY ruling."), "{plan}");

    let r = w.open();
    let (after, p) = outputs(&r.read());
    assert_eq!(holding(&after, CANARY), Vec::<String>::new());
    for files in after.values() {
        for (path, bytes) in files {
            let t = text(bytes);
            assert!(
                !t.contains("Redaction:") && !t.contains(&w.request.to_string()),
                "{path} names the redaction"
            );
        }
    }
    // each removed block of the doc reads the notice, and nothing else of it
    let plan = page(&after, "canon", "plan.html");
    // the canary prose opens the doc: its lede is the notice
    assert!(
        Regex::new(r#"<p class="lede" id="[^"]+">Content removed\.</p>"#)
            .unwrap()
            .is_match(&plan),
        "{plan}"
    );
    assert!(!plan.contains(&format!("data-rdy-block=\"{}\"", w.removed_prose)));
    for id in [
        &w.removed_section,
        &w.removed_term,
        &w.removed_figure,
        &w.image_figure,
        &w.shared_figure,
    ] {
        let at = plan
            .find(&format!("data-rdy-block=\"{id}\""))
            .unwrap_or_else(|| panic!("{id} keeps its place on the page: {plan}"));
        let end = plan[at..].find("</div>").unwrap() + at;
        assert!(
            plan[at..end].contains(NOTICE_HTML),
            "{id}: {}",
            &plan[at..end]
        );
    }
    // the decision has no place in the text; the sibling's trail no longer quotes it
    assert!(!plan.contains("Ruled:"), "{plan}");
    // a reference to a removed block reads the notice as its text, even one the author gave
    // display text
    let before_plan = page(&w.before, "canon", "plan.html");
    let also = |html: &str| {
        let at = html.find("Also <a ").unwrap_or_else(|| panic!("{html}"));
        html[at..at + html[at..].find("</a>").unwrap() + 4].to_string()
    };
    assert!(
        also(&before_plan).ends_with(">CANARY label</a>"),
        "{before_plan}"
    );
    assert!(also(&plan).ends_with(">Content removed.</a>"), "{plan}");
    // the readable blocks stay
    for kept in ["Readable sibling, edited.", "Kept page", "Plain picture"] {
        assert!(plan.contains(kept), "{kept}: {plan}");
    }
    // search, llms and the change pages read the notice
    for (scope, path) in [
        ("canon", "search-index.json"),
        ("canon", "llms-full.txt"),
        ("canon", "changes-since.html"),
        ("view:edit", "changes.html"),
    ] {
        let t = page(&after, scope, path);
        assert!(t.contains(CONTENT_REMOVED), "{scope} {path}: {t}");
    }
    // the removed question reads the notice on the collected page and in the search
    let questions = page(&after, "canon", "questions.html");
    assert!(
        questions.contains("<p><strong>Open question:</strong> Content removed.</p>"),
        "{questions}"
    );
    assert!(
        page(&w.before, "canon", "questions.html")
            .contains("<p><strong>Open question:</strong> CANARY question?</p>")
    );
    let index: Value = serde_json::from_slice(&after["canon"]["search-index.json"]).unwrap();
    let asked: Vec<&Value> = index["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "question")
        .collect();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0]["title"], CONTENT_REMOVED, "{asked:?}");
    assert_eq!(asked[0]["text"], CONTENT_REMOVED, "{asked:?}");
    let _ = &w.removed_question;
    // the review's removed line of the shared-chunk figure is the notice
    let edit = page(&after, "view:edit", "plan.html");
    assert!(
        edit.contains(&format!("<div class=\"chg-old\">{NOTICE_HTML}</div>")),
        "{edit}"
    );
    let before_edit = page(&w.before, "view:edit", "plan.html");
    assert!(
        before_edit.contains("<div class=\"chg-old\"><p class=\"figure-missing\">CANARY download"),
        "{before_edit}"
    );
    // the glossary lists no removed term
    assert!(
        !after["canon"].contains_key("glossary.html")
            || !page(&after, "canon", "glossary.html").contains(CANARY)
    );
    // every print format reads the notice, from the same page model
    assert!(
        text(&after["print"]["single.html"]).contains(CONTENT_REMOVED),
        "single.html"
    );
    let md: String = after["print"]
        .iter()
        .filter(|(k, _)| k.starts_with("md/"))
        .map(|(_, v)| text(v))
        .collect();
    assert!(md.contains(CONTENT_REMOVED), "{md}");
    let words = print_words(&p);
    assert!(!words.contains(CANARY), "{words}");
    assert!(words.contains(CONTENT_REMOVED), "{words}");
    assert!(print_words(&w.before_print).contains(CANARY));
}

/// Every word the print model holds: titles and every run.
fn print_words(p: &Print) -> String {
    fn runs(blocks: &[PBlock], out: &mut String) {
        for b in blocks {
            match b {
                PBlock::Heading { runs: r, .. } | PBlock::Para { runs: r, .. } => {
                    r.iter().for_each(|x| out.push_str(&x.text));
                }
                PBlock::List { items, .. } => items.iter().for_each(|i| runs(i, out)),
                PBlock::Table { head, rows, .. } => {
                    for c in head.iter().chain(rows.iter().flatten()) {
                        c.iter().for_each(|x| out.push_str(&x.text));
                    }
                }
                PBlock::Code { text, .. } => out.push_str(text),
                PBlock::Figure { alt, caption, .. } => {
                    out.push_str(alt);
                    caption.iter().for_each(|x| out.push_str(&x.text));
                }
                PBlock::Callout { label, blocks, .. } => {
                    out.push_str(label);
                    runs(blocks, out);
                }
            }
            out.push('\n');
        }
    }
    let mut out = format!("{}\n", p.title);
    for d in &p.docs {
        out.push_str(&d.title);
        out.push('\n');
        runs(&d.blocks, &mut out);
    }
    out
}

/// The figure blobs the print shows.
fn print_figures(p: &Print) -> BTreeSet<Hash> {
    fn walk(blocks: &[PBlock], out: &mut BTreeSet<Hash>) {
        for b in blocks {
            match b {
                PBlock::Figure { blob: Some(h), .. } => {
                    out.insert(*h);
                }
                PBlock::List { items, .. } => items.iter().for_each(|i| walk(i, out)),
                PBlock::Callout { blocks, .. } => walk(blocks, out),
                _ => {}
            }
        }
    }
    let mut out = BTreeSet::new();
    for d in &p.docs {
        walk(&d.blocks, &mut out);
    }
    out
}

#[test]
fn rdx11_removed_blobs_never_reach_a_decoder() {
    let w = world();
    let removed = [w.image_blob, w.stub_blob, w.shared_blob];
    // before: every figure's file is on the site and in the print
    let canon = &w.before["canon"];
    for h in removed.iter().chain([&w.plain_blob]) {
        assert!(
            canon.keys().any(|k| k.starts_with(&format!("assets/{h}"))),
            "{h}: {:?}",
            canon.keys()
        );
    }
    let figures = print_figures(&w.before_print);
    for h in [w.image_blob, w.stub_blob, w.plain_blob] {
        assert!(figures.contains(&h) && w.before_print.blobs.contains_key(&h));
    }

    let r = w.open();
    let st = r.read();
    // raw access to a removed blob, or to a blob that lost a shared chunk, is refused
    for h in [w.image_blob, w.shared_blob] {
        let e = RdyError::from(st.reader().get_blob(&h).unwrap_err());
        assert_eq!(e.code, E_REDACTED, "{h}");
        assert_eq!(
            e.message,
            format!("Content removed by redaction {}.", w.request)
        );
    }
    // the normal figures keep their blob fields; the removed one has none
    let m = st.materialized(&Scope::Canon).unwrap();
    assert_eq!(
        m.get(&w.image_figure).unwrap().field_str("blob"),
        Some(w.image_blob.to_string().as_str())
    );
    assert_eq!(
        m.get(&w.shared_figure).unwrap().field_str("blob"),
        Some(w.shared_blob.to_string().as_str())
    );
    assert!(m.get(&w.removed_figure).unwrap().is_redacted());
    // the site builds: no file, link or name of a removed blob, and the plain figure stays
    let (after, p) = outputs(&st);
    for (scope, files) in &after {
        for (path, bytes) in files {
            for h in &removed {
                let hex = h.to_string();
                assert!(!path.contains(&hex), "{scope}: {path}");
                assert!(!text(bytes).contains(&hex), "{scope}: {path} names {hex}");
            }
        }
    }
    let canon = &after["canon"];
    assert!(
        canon
            .keys()
            .any(|k| k.starts_with(&format!("assets/{}", w.plain_blob)))
    );
    let plan = page(&after, "canon", "plan.html");
    assert!(!plan.contains("figure-missing"), "{plan}");
    assert!(!plan.contains("not available"), "{plan}");
    assert!(plan.contains("alt=\"Plain picture\""), "{plan}");
    assert!(
        plan.contains(&format!("data-rdy-block=\"{}\"", w.plain_figure)),
        "{plan}"
    );
    // the print holds only the plain figure: no removed blob reaches its image decoder
    assert_eq!(print_figures(&p), BTreeSet::from([w.plain_blob]));
    assert_eq!(
        p.blobs.keys().copied().collect::<Vec<_>>(),
        vec![w.plain_blob]
    );
    let (_, _, warnings) = print::pdf::pdf_bytes(&p, &Fonts::load());
    assert_eq!(warnings, Vec::<String>::new());
}

#[test]
fn rdx11_unaffected_children_keep_links_and_pages() {
    let w = world();
    // before: the folder and the doc are named by their names
    let canon = &w.before["canon"];
    assert!(canon.contains_key("vault/kept.html") && canon.contains_key("secret.html"));

    let r = w.open();
    let (after, _) = outputs(&r.read());
    let canon = &after["canon"];
    let folder_page = format!("{}/index.html", w.folder);
    let kept_page = format!("{}/kept.html", w.folder);
    let doc_page = format!("{}.html", w.removed_doc);
    for p in [&folder_page, &kept_page, &doc_page] {
        assert!(canon.contains_key(p), "{p}: {:?}", canon.keys());
    }
    assert!(!canon.contains_key("vault/kept.html") && !canon.contains_key("secret.html"));
    // the kept doc's page: its own words, and the folder above it read as the notice
    let kept = page(&after, "canon", &kept_page);
    assert!(kept.contains("Kept text under the folder."), "{kept}");
    assert!(kept.contains("<title>Kept page"), "{kept}");
    assert!(kept.contains("Content removed."), "{kept}");
    // the removed folder's page leads to the kept doc
    let folder = page(&after, "canon", &folder_page);
    assert!(folder.contains("href=\"kept.html\""), "{folder}");
    assert!(folder.contains("<title>Content removed."), "{folder}");
    // the removed doc's page keeps its kept section and text
    let doc = page(&after, "canon", &doc_page);
    assert!(doc.contains("<title>Content removed."), "{doc}");
    assert!(
        doc.contains("Kept section") && doc.contains("Kept child text."),
        "{doc}"
    );
    // links and permalinks to the kept blocks lead to their pages
    let plan = page(&after, "canon", "plan.html");
    assert!(plan.contains(&format!("href=\"{kept_page}\"")), "{plan}");
    for (id, target) in [
        (&w.kept_doc, kept_page.clone()),
        (&w.kept_section, doc_page.clone()),
        (&w.folder, folder_page.clone()),
        (&w.removed_doc, doc_page.clone()),
    ] {
        let go = page(&after, "canon", &format!("go/{id}.html"));
        assert!(go.contains(&format!("../{target}")), "{id}: {go}");
        assert!(!go.contains(CANARY), "{go}");
    }
    // the home page and the navigation name the removed folder and doc by the notice
    let home = page(&after, "canon", "index.html");
    assert!(home.contains(&format!("href=\"{doc_page}\"")), "{home}");
    assert!(home.contains(&format!("href=\"{folder_page}\"")), "{home}");
    assert!(!home.contains(CANARY), "{home}");
    // unaffected blocks keep their pages and words
    let _ = (&w.plan, &w.links, &w.sibling, &w.decision);
}

#[test]
fn rdx11_tags_show_only_the_redacted_badge() {
    let w = world();
    let home = page(&w.before, "canon", "index.html");
    assert!(!home.contains("redacted"), "{home}");

    let r = w.open();
    let st = r.read();
    let (after, _) = outputs(&st);
    // the home page's versions: v1.0.0 carries the badge, v0.1.0 none
    let home = page(&after, "canon", "index.html");
    let versions = &home[home.find("id=\"versions\"").unwrap()..];
    let v1 = &versions[versions.find("<dt>v1.0.0").unwrap()..];
    let v1 = &v1[..v1.find("</dt>").unwrap()];
    assert!(v1.contains("redacted"), "{v1}");
    let v0 = &versions[versions.find("<dt>v0.1.0").unwrap()..];
    let v0 = &v0[..v0.find("</dt>").unwrap()];
    assert!(!v0.contains("redacted"), "{v0}");
    // the live menu: the same badge on the same version only
    let m = menu::menu(&st, &mut |s| menu::docs_changed(&st, s)).unwrap();
    let row = |name: &str| m.versions.iter().find(|r| r.label == name).unwrap();
    assert!(row("v1.0.0").meta.contains(&"redacted".to_string()));
    assert!(!row("v0.1.0").meta.contains(&"redacted".to_string()));
    // no reason, actor, approval words, request or card reaches the site or the print
    let approval = Proof::relayed_human();
    let question = approval.relay_evidence.unwrap().question.unwrap();
    for word in [
        REASON,
        "agent:requester",
        "agent:relay",
        "human:alice",
        question.as_str(),
        "Request content removal.",
        "Approve",
        "Redaction",
        "secret",
        &w.request.to_string(),
    ] {
        assert_eq!(holding(&after, word), Vec::<String>::new(), "{word}");
    }
}

// --- reading a PDF back ------------------------------------------------------------------------

/// One object of a PDF: its dictionary's text and its stream's bytes.
struct Obj {
    dict: String,
    stream: Option<Vec<u8>>,
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
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
        // the object number is the digits before
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

fn inflate(o: &Obj) -> Vec<u8> {
    let data = o.stream.as_ref().expect("a stream");
    if o.dict.contains("/Filter /FlateDecode") {
        miniz_oxide::inflate::decompress_to_vec_zlib(data).expect("the stream inflates")
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

/// A content stream's strings and operators, in order (names and numbers as operators too:
/// only `Tf`'s name and `Tj`'s string are read).
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
                            let t = map.get(&g);
                            out.push_str(t.unwrap_or_else(|| panic!("glyph {g} is in ToUnicode")));
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

#[test]
fn rdx11_pdf_text_and_images_prove_removal() {
    let w = world();
    let before = &w.before["print"]["print.pdf"];
    let text_before = pdf_text(before);
    assert!(text_before.contains(CANARY), "{text_before}");
    assert!(text_before.contains("Readable sibling, edited."));
    let images_before = pdf_images(before);
    for dims in [(7, 5), (9, 4), (3, 2)] {
        assert!(images_before.contains(&dims), "{dims:?}: {images_before:?}");
    }

    let r = w.open();
    let (after, _) = outputs(&r.read());
    let pdf = &after["print"]["print.pdf"];
    let text_after = pdf_text(pdf);
    assert!(!text_after.contains(CANARY), "{text_after}");
    // nor do the file's raw bytes (its outline and info strings among them)
    assert!(find(pdf, CANARY.as_bytes(), 0).is_none());
    assert!(text_after.contains(CONTENT_REMOVED), "{text_after}");
    assert!(text_after.contains("Readable sibling, edited."));
    assert!(text_after.contains("Kept child text."));
    assert_eq!(pdf_images(pdf), BTreeSet::from([(3, 2)]));
}

/// The words a change page marks as removed (`<del>`), the screen-reader word left out.
fn deleted_words(html: &str) -> String {
    Regex::new(r#"<del><span class="sr-word">removed: </span>(.*?)</del>"#)
        .unwrap()
        .captures_iter(html)
        .map(|c| c[1].to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A removed doc is named by its id (Task 3), so a sibling whose name is that id ignoring case
/// would be the same file on a file system that ignores case. Since POST-LAUNCH 52 a doc or
/// folder name that is a `d-` or `fd-` id ignoring case is refused at write (E_BAD_NAME), so that
/// sibling cannot be made: each such write is refused with the name diagnostic and writes
/// nothing. The render's neutral-path clash refusal (E_PATH_CLASH, the last site kept) stays as
/// defence in depth; a unit test in `site.rs` pins it on a synthetic state.
#[test]
fn rdx11_a_neutral_path_that_matches_a_sibling_refuses_and_keeps_the_last_site() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "d-SECRT", "fields": {"name": "secret", "title": "CANARY doc title", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "d-SECRT", "body": "Kept text of the removed doc."}
        ]),
    );
    let removed = bid("d-SECRT");
    let state = |r: &mut Rdy| {
        let end = format!("{:?}", r.store().log_end());
        let views = r.read().views_with_changes();
        let ids: Vec<String> = r
            .read()
            .materialized(&Scope::View("w1".into()))
            .unwrap()
            .ids()
            .map(|i| i.as_str().to_string())
            .collect();
        (end, views, ids)
    };
    let why = "a name cannot look like a block id (it would be read as one)";
    // the id lower-cased, the id itself, and a mixed case: each names the removed doc's file
    for bad in ["d-secrt", "D-SECRT", "D-secrt"] {
        assert!(bad.eq_ignore_ascii_case(removed.as_str()));
        let before = state(&mut r);
        let e = r
            .write(
                &agent(),
                WriteRequest {
                    view: "w1".into(),
                    ops: serde_json::from_value(json!([
                        {"op": "create", "type": "doc", "id": "$b", "fields": {"name": bad, "title": "Neighbour", "doc_type": "page"}},
                        {"op": "create", "type": "prose", "parent": "$b", "body": "Neighbour text."}
                    ]))
                    .unwrap(),
                    justification: None,
                    message: "write to w1".into(),
                },
            )
            .unwrap_err();
        assert_eq!(e.code, E_BAD_NAME, "{bad}");
        assert_eq!(e.message, format!("{bad:?} is not a valid name: {why}"));
        let fixed = e.corrected.as_ref().unwrap()["fields"]["name"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(fixed.starts_with("untitled-"), "{fixed}");
        assert_eq!(e.fix, format!("use a name like {fixed:?}"));
        assert_eq!(
            e.corrected,
            Some(json!({"op": 0, "fields": {"name": fixed}}))
        );
        assert_eq!(state(&mut r), before, "{bad}: nothing written");
    }
}

/// Ruling task-2: a draft replaces a figure's file and its alt text with a file that shares a
/// chunk with another file; then a redaction removes that other file. The projection then marks
/// the figure itself removed (its anchor is `content-removed`), so it reads the notice. It is an
/// Edited item of the draft's page, and its label in "Changes on this page" reads the notice too,
/// never its alt text. The pin guards the whole pipeline, from the projection to the group, not
/// only the `shows_removed` branch of the group's label.
#[test]
fn rvm2_a_removed_items_label_in_the_changes_group_reads_the_notice() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let old_blob = r
        .store_mut()
        .put_blob(&rgb_png(7, 5, [200, 30, 30]))
        .unwrap();
    let removed_bytes = vec![7u8; 1 << 20];
    let mut shared_bytes = removed_bytes.clone();
    shared_bytes.extend_from_slice(b"tail");
    let removed_blob = r.store_mut().put_blob(&removed_bytes).unwrap();
    let new_blob = r.store_mut().put_blob(&shared_bytes).unwrap();
    let w = write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "Kept text."},
            {"op": "create", "type": "figure", "id": "$f", "parent": "$d", "fields": {"blob": old_blob.to_string(), "media_type": "image/png", "alt": "Safe old image"}}
        ]),
    );
    let figure = bid(&w.created["$f"]);
    land(&mut r, "w1");
    write(
        &mut r,
        "edit",
        json!([{"op": "update", "id": figure.to_string(), "fields": {"blob": new_blob.to_string(), "media_type": "application/octet-stream", "alt": "CANARY new download"}}]),
    );
    let group = |plan: &str| {
        let at = plan
            .find("aria-label=\"Changes on this page\"")
            .unwrap_or_else(|| panic!("the changes group: {plan}"));
        plan[at..at + plan[at..].find("</nav>").unwrap()].to_string()
    };
    let before = text(&site(&r.read(), "view:edit")["plan.html"]);
    assert!(group(&before).contains("CANARY new download"), "{before}");
    r.close().unwrap();
    let fixture = authorized_batch_fixture(
        &dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![RedactTarget::Blob(removed_blob)],
        )],
        &FixtureOptions::approved(Proof::relayed_human()),
    );
    assert_eq!(
        fixture.requests[0].draft.request.manifest.shared_blobs,
        vec![new_blob],
        "the figure's file shares the removed chunk"
    );

    let r = Rdy::open(dir.path()).unwrap();
    let plan = text(&site(&r.read(), "view:edit")["plan.html"]);
    let at = plan
        .find(&format!("data-rdy-block=\"{figure}\""))
        .unwrap_or_else(|| panic!("the figure keeps its place: {plan}"));
    assert!(
        plan[at..at + plan[at..].find("</div>").unwrap()].contains(NOTICE_HTML),
        "the figure itself reads the notice: {plan}"
    );
    let g = group(&plan);
    assert!(g.contains("chg-edited"), "{g}");
    assert!(
        g.contains(&format!("edited</span> {CONTENT_REMOVED}</a>")),
        "{g}"
    );
    assert!(!plan.contains(CANARY), "{plan}");
}

/// RVM final review M2: a term edited in a view, then that version removed. A term's anchor is on
/// its `<dt>`, which the notice replaces, so a removed term's block holds its anchor: its entry
/// in "Changes on this page" and its Copy link jump to an element that exists, and only one
/// element holds that id.
#[test]
fn rvm2_a_removed_terms_entry_jumps_to_its_block() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "Kept text."},
            {"op": "create", "type": "term", "id": "$t", "parent": "$d", "fields": {"name": "Widget"}, "body": "A widget."}
        ]),
    );
    let term = bid(&w.created["$t"]);
    land(&mut r, "w1");
    write(
        &mut r,
        "edit",
        json!([{"op": "update", "id": term.to_string(), "body": "A widget, CANARY edited."}]),
    );
    let st = r.read();
    let m = st
        .materialized(&st.resolve_scope("view:edit").unwrap())
        .unwrap();
    let version = m.get(&term).unwrap().version_hash();
    drop(st);
    r.close().unwrap();
    authorized_batch_fixture(
        &dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![RedactTarget::Version {
                id: term.clone(),
                version,
            }],
        )],
        &FixtureOptions::approved(Proof::relayed_human()),
    );

    let r = Rdy::open(dir.path()).unwrap();
    let plan = text(&site(&r.read(), "view:edit")["plan.html"]);
    assert!(!plan.contains(CANARY), "{plan}");
    let at = plan
        .find("aria-label=\"Changes on this page\"")
        .unwrap_or_else(|| panic!("the changes group: {plan}"));
    let g = &plan[at..at + plan[at..].find("</nav>").unwrap()];
    let entry = g
        .split("<li><a class=\"toc-link toc-chg\" href=\"#")
        .nth(1)
        .unwrap_or_else(|| panic!("the term's entry: {g}"));
    assert!(
        entry.contains(&format!("edited</span> {CONTENT_REMOVED}</a>")),
        "{entry}"
    );
    let href = &entry[..entry.find('"').unwrap()];
    // the elements that hold the id, leaving out the id-anchor spans
    let holders: Vec<&str> = plan
        .match_indices(&format!(" id=\"{href}\""))
        .map(|(i, _)| {
            let s = plan[..i].rfind('<').unwrap();
            &plan[s..s + plan[s..].find('>').unwrap()]
        })
        .filter(|tag| !tag.contains("id-anchor"))
        .collect();
    assert_eq!(holders.len(), 1, "#{href}: {holders:?}\n{plan}");
    assert!(
        holders[0].contains(&format!("data-rdy-block=\"{term}\"")),
        "#{href} is the term's block: {}",
        holders[0]
    );
    assert!(
        plan.contains(&format!("data-href=\"#{href}\"")),
        "the term's Copy link names its anchor: {plan}"
    );
}

/// A draft replaces a figure's file and its alt text; then a redaction removes the old file. The
/// draft's change summary reads the old side as the notice (removed words), never as an empty
/// figure or its old alt text.
#[test]
fn rdx11_an_edited_figure_whose_base_blob_was_removed_diffs_from_the_notice() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let old_blob = r
        .store_mut()
        .put_blob(&rgb_png(7, 5, [200, 30, 30]))
        .unwrap();
    let new_blob = r
        .store_mut()
        .put_blob(&rgb_png(3, 2, [30, 30, 200]))
        .unwrap();
    let w = write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "Kept text."},
            {"op": "create", "type": "figure", "id": "$f", "parent": "$d", "fields": {"blob": old_blob.to_string(), "media_type": "image/png", "alt": "CANARY old image"}}
        ]),
    );
    let figure = bid(&w.created["$f"]);
    land(&mut r, "w1");
    write(
        &mut r,
        "edit",
        json!([{"op": "update", "id": figure.to_string(), "fields": {"blob": new_blob.to_string(), "alt": "Safe new image"}}]),
    );
    let before = site(&r.read(), "view:edit");
    let changes = text(&before["changes.html"]);
    assert!(deleted_words(&changes).contains("CANARY"), "{changes}");
    r.close().unwrap();
    authorized_batch_fixture(
        &dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![RedactTarget::Blob(old_blob)],
        )],
        &FixtureOptions::approved(Proof::relayed_human()),
    );

    let r = Rdy::open(dir.path()).unwrap();
    let after = site(&r.read(), "view:edit");
    assert!(
        holding(
            &BTreeMap::from([("edit".to_string(), after.clone())]),
            CANARY
        )
        .is_empty()
    );
    let changes = text(&after["changes.html"]);
    assert!(
        deleted_words(&changes).contains(CONTENT_REMOVED),
        "the old figure reads the notice: {changes}"
    );
    assert!(changes.contains("Safe new image"), "{changes}");
    let plan = text(&after["plan.html"]);
    assert!(plan.contains("Safe new image"), "{plan}");
}
