//! The M3 gate's publication fixture (mg6): two selected docs, one excluded doc, a public and an
//! org profile, a selected and an excluded figure, a selected and an outside decision and a
//! missing target. Every excluded field and asset holds its own canary.
//!
//! The expectations here come from how the fixture is built, never from an engine answer: the
//! counted references, the hidden link edge, the dropped decision citations and the canaries.
//! [`drive`] runs the chat tools through a live MCP client and freezes what they returned and
//! wrote. [`audit`] reads a frozen result against the expectations; it calls no engine.
//! [`scan_local_links`] resolves every local link and fragment against the produced file set.
//!
//! Nothing here fetches an external address.
use crate::daemon::short_tempdir;
use crate::fixtures::{approver, land, land_with_a_tie, raw, write, writer};
use crate::mcp::{McpClient, Reply};
use rdy_engine::Rdy;
use rdy_model::{Change, ChangeLinks, Op};
use rdy_render::print::fonts::Fonts;
use rdy_render::print::layout::LItem;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The tag the fixture lands first.
pub const TAG: &str = "v1.0.0";

/// The words a reference outside the selection with no display text reads as.
pub const UNAVAILABLE: &str = "unavailable reference";

/// The id the missing target would have had. No block of the project holds it.
pub const MISSING_TARGET: &str = "p-ZZZZZ";

/// A set of produced files, by their path below the output root.
pub type Files = BTreeMap<String, Vec<u8>>;

/// One reference row the check must report: source, location, target token, classification and
/// occurrences.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Row {
    pub source: String,
    pub location: String,
    pub target: String,
    pub classification: &'static str,
    pub occurrences: u64,
}

impl Row {
    fn new(source: &str, location: &str, target: &str, class: &'static str, n: u64) -> Row {
        Row {
            source: source.into(),
            location: location.into(),
            target: target.into(),
            classification: class,
            occurrences: n,
        }
    }

    /// The row as the check reports it.
    pub fn json(&self) -> Value {
        json!({
            "source": self.source,
            "location": self.location,
            "target": self.target,
            "classification": self.classification,
            "occurrences": self.occurrences,
        })
    }
}

/// What the fixture built, and what follows from how it was built.
#[derive(Clone, Debug)]
pub struct Scenario {
    /// Each fixture block by its placeholder, with its id.
    pub ids: BTreeMap<String, String>,
    /// The ids both profiles must select: the two docs with their blocks, the decision that
    /// governs a selected claim, and the question that decision answers.
    pub selected: BTreeSet<String>,
    /// The ids no profile may select, or show.
    pub excluded: BTreeSet<String>,
    /// The ids of the two profile blocks.
    pub profiles: BTreeMap<String, String>,
    /// The landing the tag names.
    pub landing: String,
    /// Words an org output shows because the author chose them for a selected block.
    pub selected_words: Vec<String>,
    /// Words no profile output may hold in any file name, byte, model or drawn text.
    pub excluded_words: Vec<String>,
    /// The bytes of the selected and of the excluded figure.
    pub selected_png: Vec<u8>,
    pub excluded_png: Vec<u8>,
    /// The blob hash of each figure.
    pub selected_blob: String,
    pub excluded_blob: String,
    /// The counted rows of the source state (excluded and missing), sorted.
    pub rows: Vec<Row>,
    /// The link edges that leave the selection, sorted.
    pub link_rows: Vec<Row>,
    /// The outside decisions that a reference names by id: dropped, never counted.
    pub dropped_decisions: Vec<String>,
    /// The words the outputs read for the decision citations the author wrote in parentheses.
    pub citation_words: String,
}

impl Scenario {
    /// The id of the fixture block `name` (a placeholder such as `$xp`).
    pub fn id(&self, name: &str) -> &str {
        self.ids
            .get(name)
            .unwrap_or_else(|| panic!("no fixture block {name}"))
    }

    /// The counted references of the source state: excluded occurrences, then missing ones.
    pub fn counted(&self) -> (u64, u64) {
        let sum = |class: &str| {
            self.rows
                .iter()
                .filter(|r| r.classification == class)
                .map(|r| r.occurrences)
                .sum()
        };
        (sum("excluded"), sum("missing"))
    }
}

/// An 8-bit RGB PNG of `w` x `h`, its one IDAT stored (no compression).
pub fn png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
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
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let line: Vec<u8> = std::iter::once(0)
        .chain(rgb.iter().copied().cycle().take(3 * w as usize))
        .collect();
    let raw = line.repeat(h as usize);
    let (mut a, mut b) = (1u32, 0u32);
    for x in &raw {
        a = (a + u32::from(*x)) % 65521;
        b = (b + a) % 65521;
    }
    let mut z = vec![0x78, 0x01, 1];
    let len = raw.len() as u16;
    z.extend_from_slice(&len.to_le_bytes());
    z.extend_from_slice(&(!len).to_le_bytes());
    z.extend_from_slice(&raw);
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
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

/// Standard base64 with padding: how the single HTML file embeds a figure.
pub fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Builds the fixture in the project at `dir` (already initialized, closed on return).
///
/// Canon: the docs `alpha` and `bravo` (selected), the doc `xdoccanaryname` (excluded, and the
/// home of both profiles), a decision that governs a selected claim and the question it answers
/// (selected through the root rule), and an outside decision and question (not selected). Then
/// one block raw-committed with a reference to a block the project never had, the two profiles
/// `pubp` (public) and `orgp` (org, with a trail), and the tag [`TAG`] with a message canary.
pub fn seed(dir: &Path) -> Scenario {
    let mut r = Rdy::open(dir).expect("open the fixture project");
    let selected_png = png(5, 3, [10, 120, 200]);
    let excluded_png = png(4, 7, [201, 33, 44]);
    let selected_blob = r.store_mut().put_blob(&selected_png).unwrap();
    let excluded_blob = r.store_mut().put_blob(&excluded_png).unwrap();
    let w = write(
        &mut r,
        "w1",
        json!([
            // the excluded doc, with every kind of canary
            {"op": "create", "type": "doc", "id": "$x", "fields": {"name": "xdoccanaryname", "title": "XDOCCANARY title", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$xs", "parent": "$x", "fields": {"title": "XSECCANARY section"}},
            {"op": "create", "type": "prose", "id": "$xp", "parent": "$xs", "body": "XPROSECANARY words."},
            {"op": "create", "type": "term", "id": "$xt", "parent": "$xs", "fields": {"name": "XTERMCANARY", "key": "XKEY-CANARY"}, "body": "XTERMCANARY definition."},
            {"op": "create", "type": "figure", "id": "$xf", "parent": "$xs", "fields": {"blob": excluded_blob.to_string(), "media_type": "image/png", "alt": "XFIGCANARY alt"}},
            // the decisions and questions with no doc
            {"op": "create", "type": "decision", "id": "$decOut", "fields": {"statement": "OUTDECCANARY ruling.", "status": "proposed", "key": "out-rule"}},
            {"op": "create", "type": "question", "id": "$qOut", "fields": {"prompt": "OUTQCANARY question?"}},
            {"op": "create", "type": "question", "id": "$qHop", "fields": {"prompt": "HOPQCANARY question?"}},
            {"op": "create", "type": "decision", "id": "$decSel",
             "fields": {"statement": "SELDECCANARY ruling.", "status": "proposed", "key": "sel-rule"},
             "links": {"answers": [{"target": "$qHop"}]}},
            // the second selected doc
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "bravo", "title": "Bravo guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$sB", "parent": "$b", "fields": {"title": "Bravo section"}},
            {"op": "create", "type": "prose", "id": "$pB1", "parent": "$sB", "body": "BRAVOKEEP words. Go [ALPHAPAGE](alpha.html) and [ALPHALINK](alpha.html#alpha-section) and [OWNLINK](#bravo-section) and [HIDDENLINK](xdoccanaryname.html) now."},
            {"op": "create", "type": "term", "id": "$tB", "parent": "$sB", "fields": {"name": "Bravo term", "key": "bravo-key"}, "body": "BRAVOTERM definition."},
            // the first selected doc
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "alpha", "title": "Alpha guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$sA", "parent": "$a", "fields": {"title": "Alpha section {{$xs}}"}},
            {"op": "create", "type": "prose", "id": "$pA1", "parent": "$sA", "body": "ALPHAKEEP words. A selected {{$pB1|Bravo display words}} and bare {{$pB1}}."},
            {"op": "create", "type": "prose", "id": "$pA2", "parent": "$sA", "body": "Explicit {{$xs|Shown display words}} and bare {{$xs}} and typed {{$xt|@key}}."},
            {"op": "create", "type": "prose", "id": "$pA3", "parent": "$sA", "body": "The rule holds ({{$decOut|@key}}, {{$decOut}}). Named {{$decOut|the outside rule}} stays. Spaced ( {{$decOut}} ;  ) too. Chosen {{$decSel|@key}} and bare {{$decSel}}."},
            {"op": "create", "type": "table", "id": "$tA", "parent": "$sA", "fields": {"columns": ["Label {{$xs}}"], "rows": [["Cell {{$xs}}"]]}},
            {"op": "create", "type": "figure", "id": "$fA", "parent": "$sA", "fields": {"blob": selected_blob.to_string(), "media_type": "image/png", "alt": "Selected figure alt {{$xs}}"}},
            {"op": "create", "type": "claim", "id": "$cA", "parent": "$sA", "body": "ALPHACLAIM governed words.",
             "links": {"decided_by": [{"target": "$decSel"}], "depends_on": [{"target": "$xp"}]}}
        ]),
    )
    .unwrap_or_else(|e| panic!("write the fixture: {e}"));
    let mut ids: BTreeMap<String, String> = w.created.clone().into_iter().collect();
    land(&mut r, "w1").unwrap_or_else(|e| panic!("land the fixture: {e}"));

    // a reference to no block of the project: the write path refuses one, so it goes in raw
    let missing = raw(
        "p-M1SS1",
        "prose",
        Some(&ids["$sB"]),
        "zz",
        &[],
        &format!("Missing {{{{{MISSING_TARGET}}}}} here."),
    );
    let change = Change {
        view: "raw".into(),
        ops: vec![Op::Create { block: missing }],
        deps: vec![],
        provenance: writer().provenance(),
        justification: None,
        message: "raw".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    };
    r.store_mut().commit_to_view("raw", change).unwrap();
    r.invalidate();
    land_with_a_tie(&mut r, "raw");
    ids.insert("$pM".into(), "p-M1SS1".into());

    let p = write(
        &mut r,
        "w2",
        json!([
            {"op": "create", "type": "publication_profile", "id": "$pubp", "parent": ids["$x"],
             "fields": {"key": "pubp", "title": "Title of pubp", "description": "Description of pubp.",
                        "audience": "public", "select": [ids["$a"], ids["$b"]]}},
            {"op": "create", "type": "publication_profile", "id": "$orgp", "parent": ids["$x"],
             "fields": {"key": "orgp", "title": "Title of orgp", "description": "Description of orgp.",
                        "audience": "org", "trail": true, "select": [ids["$a"], ids["$b"]]}}
        ]),
    )
    .unwrap_or_else(|e| panic!("write the profiles: {e}"));
    land(&mut r, "w2").unwrap_or_else(|e| panic!("land the profiles: {e}"));
    let profiles: BTreeMap<String, String> = [
        ("pubp".to_string(), p.created["$pubp"].clone()),
        ("orgp".to_string(), p.created["$orgp"].clone()),
    ]
    .into();
    let tag = r
        .tag(&approver(), TAG, None, Some("TAGMSGCANARY".into()))
        .unwrap_or_else(|e| panic!("tag: {e}"));
    r.close().unwrap();

    let id = |k: &str| ids[k].clone();
    let selected: BTreeSet<String> = [
        "$a", "$sA", "$pA1", "$pA2", "$pA3", "$tA", "$fA", "$cA", "$b", "$sB", "$pB1", "$tB",
        "$pM", "$decSel", "$qHop",
    ]
    .iter()
    .map(|k| id(k))
    .collect();
    let excluded: BTreeSet<String> = ["$x", "$xs", "$xp", "$xt", "$xf", "$decOut", "$qOut"]
        .iter()
        .map(|k| id(k))
        .chain(profiles.values().cloned())
        .collect();

    let mut rows = vec![
        Row::new(&id("$pA2"), "body", &id("$xs"), "excluded", 2),
        Row::new(&id("$pA2"), "body", &id("$xt"), "excluded", 1),
        Row::new(&id("$sA"), "/fields/title", &id("$xs"), "excluded", 1),
        Row::new(&id("$tA"), "/fields/columns/0", &id("$xs"), "excluded", 1),
        Row::new(&id("$tA"), "/fields/rows/0/0", &id("$xs"), "excluded", 1),
        Row::new(&id("$fA"), "/fields/alt", &id("$xs"), "excluded", 1),
        Row::new(&id("$pM"), "body", MISSING_TARGET, "missing", 1),
    ];
    rows.sort();
    let link_rows = vec![Row::new(
        &id("$cA"),
        "/links/depends_on/0",
        &id("$xp"),
        "excluded",
        1,
    )];

    // every word whose absence proves a field or an asset stayed out
    let mut excluded_words: Vec<String> = [
        "XDOCCANARY",
        "xdoccanaryname",
        "XSECCANARY",
        "XPROSECANARY",
        "XTERMCANARY",
        "XKEY-CANARY",
        "xkey-canary",
        "XFIGCANARY",
        "OUTDECCANARY",
        "out-rule",
        "OUTQCANARY",
        "TAGMSGCANARY",
        MISSING_TARGET,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    excluded_words.extend(excluded.iter().cloned());
    excluded_words.push(excluded_blob.to_string());
    excluded_words.push(tag.landing.clone());

    let selected_words = [
        "ALPHAKEEP",
        "BRAVOKEEP",
        "BRAVOTERM",
        "ALPHACLAIM",
        "ALPHAPAGE",
        "SELDECCANARY",
        "Shown display words",
        "the outside rule",
        "Selected figure alt",
        "Description of orgp.",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let dropped_decisions = vec![id("$decOut")];
    Scenario {
        ids,
        selected,
        excluded,
        profiles,
        landing: tag.landing,
        selected_words,
        excluded_words,
        selected_png,
        excluded_png,
        selected_blob: selected_blob.to_string(),
        excluded_blob: excluded_blob.to_string(),
        rows,
        link_rows,
        dropped_decisions,
        citation_words: "The rule holds. Named the outside rule stays. Spaced too.".into(),
    }
}

// --- files ---------------------------------------------------------------------------------------

/// Every file below `dir`, by its path relative to `dir`. A single file is held under its own
/// name. Missing: empty.
pub fn tree(dir: &Path) -> Files {
    fn walk(root: &Path, at: &Path, out: &mut Files) {
        for e in std::fs::read_dir(at).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                out.insert(rel.replace('\\', "/"), std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = Files::new();
    if dir.is_file() {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        out.insert(name, std::fs::read(dir).unwrap());
    } else if dir.is_dir() {
        walk(dir, dir, &mut out);
    }
    out
}

/// The paths whose name or bytes hold `word`.
pub fn holding(files: &Files, word: &[u8]) -> Vec<String> {
    if word.is_empty() {
        return vec![];
    }
    files
        .iter()
        .filter(|(path, bytes)| {
            path.as_bytes().windows(word.len()).any(|w| w == word)
                || bytes.windows(word.len()).any(|w| w == word)
        })
        .map(|(path, _)| path.clone())
        .collect()
}

/// The text of the model of `profile` at `scope` and of the text its PDF draws, read from the
/// project at `dir` (which no daemon may hold). A PDF's text streams are compressed, so its raw
/// bytes alone prove nothing; this is the text a reader would see on the pages.
pub fn print_text(dir: &Path, scope: &str, profile: &str) -> String {
    let r = Rdy::open(dir).expect("open the project to read the print model");
    let st = r.read();
    let print = rdy_render::print::build_profile(&st, scope, profile)
        .unwrap_or_else(|e| panic!("print model of {profile} at {scope}: {e}"));
    let words = rdy_render::print::pdf::lay_out(&print, &Fonts::load())
        .pages
        .iter()
        .flat_map(|page| {
            page.items.iter().filter_map(|it| match it {
                LItem::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
        })
        .collect::<Vec<_>>()
        .join(" ");
    let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{print:?}\n{words}")
}

// --- the local link checker ----------------------------------------------------------------------

/// What [`scan_local_links`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkScan {
    /// Local references examined (external addresses are not).
    pub checked: usize,
    /// One line per local reference that does not resolve: file, reference, reason.
    pub broken: Vec<String>,
}

/// The values of the attribute `name` in `html`, only where a space, a tab or a line break
/// precedes it (so `data-id` is no `id`).
fn attr_values(html: &str, name: &str) -> Vec<String> {
    let key = format!("{name}=\"");
    let bytes = html.as_bytes();
    let mut out = vec![];
    let mut from = 0;
    while let Some(i) = html[from..].find(&key) {
        let at = from + i;
        let value = at + key.len();
        let space = at > 0 && matches!(bytes[at - 1], b' ' | b'\t' | b'\n' | b'\r');
        match html[value..].find('"') {
            Some(j) => {
                if space {
                    out.push(crate::site::unescape(&html[value..value + j]));
                }
                from = value + j + 1;
            }
            None => break,
        }
    }
    out
}

/// `%XX` decoded to bytes, read as UTF-8; None for a `%` not followed by two hex digits or bytes
/// that are no UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Whether `t` starts with a URL scheme (`http:`, `mailto:`, `data:`) or is network-relative.
fn is_external(t: &str) -> bool {
    if t.starts_with("//") {
        return true;
    }
    let Some(colon) = t.find(':') else {
        return false;
    };
    let scheme = &t[..colon];
    !scheme.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// The targets of Markdown links and images in `text`: the text between `](` and its `)`.
fn markdown_targets(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = text;
    while let Some(i) = rest.find("](") {
        rest = &rest[i + 2..];
        let Some(j) = rest.find(')') else { break };
        let t = rest[..j].trim();
        let t = t.trim_start_matches('<');
        let t = t.split_once(" \"").map_or(t, |(a, _)| a);
        out.push(t.trim_end_matches('>').to_string());
        rest = &rest[j..];
    }
    out
}

/// The directory part of `path` (empty at the root).
fn dir_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

/// `base` (a directory) joined with `rel`, with `.` and `..` folded; None above the root.
fn join(base: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = if rel.starts_with('/') || base.is_empty() {
        vec![]
    } else {
        base.split('/').collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// Resolves every local link, image, stylesheet and fragment in the HTML, Markdown and text
/// files of `files` against the file set itself. A relative path is read from the file's own
/// directory and percent-decoded; a directory means its `index.html`; a fragment must be an
/// `id` or `name` of the target HTML file. External addresses are not fetched and not counted.
pub fn scan_local_links(files: &Files) -> LinkScan {
    let text = |p: &str| String::from_utf8_lossy(&files[p]).into_owned();
    let mut ids: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for path in files.keys().filter(|p| p.ends_with(".html")) {
        let html = text(path);
        let mut set: BTreeSet<String> = attr_values(&html, "id").into_iter().collect();
        set.extend(attr_values(&html, "name"));
        ids.insert(path.as_str(), set);
    }
    let mut scan = LinkScan::default();
    for path in files.keys() {
        let body = text(path);
        let targets: Vec<String> = if path.ends_with(".html") {
            ["href", "src"]
                .iter()
                .flat_map(|a| attr_values(&body, a))
                .collect()
        } else if path.ends_with(".md") || path.ends_with(".txt") {
            markdown_targets(&body)
        } else {
            continue;
        };
        for target in targets {
            if is_external(&target) {
                continue;
            }
            scan.checked += 1;
            let mut fail = |why: &str| scan.broken.push(format!("{path}: {target}: {why}"));
            let (before_query, _) = target.split_once('?').unwrap_or((&target, ""));
            let (file_part, frag) = match before_query.split_once('#') {
                Some((f, g)) => (f, Some(g)),
                None => (before_query, None),
            };
            let Some(decoded) = percent_decode(file_part) else {
                fail("bad percent-encoding in the path");
                continue;
            };
            let resolved = if decoded.is_empty() {
                Some(path.clone())
            } else {
                join(dir_of(path), &decoded)
            };
            let Some(resolved) = resolved else {
                fail("leaves the output root");
                continue;
            };
            let found = if !resolved.is_empty() && files.contains_key(&resolved) {
                Some(resolved)
            } else {
                let index = if resolved.is_empty() {
                    "index.html".to_string()
                } else {
                    format!("{resolved}/index.html")
                };
                files.contains_key(&index).then_some(index)
            };
            let Some(file) = found else {
                fail("no such file in the output");
                continue;
            };
            let Some(frag) = frag.filter(|f| !f.is_empty() && *f != "top") else {
                continue;
            };
            if !file.ends_with(".html") {
                continue;
            }
            let Some(want) = percent_decode(frag) else {
                fail("bad percent-encoding in the fragment");
                continue;
            };
            if !ids[file.as_str()].contains(&want) {
                fail(&format!("no id {want} in {file}"));
            }
        }
    }
    scan
}

// --- the frozen result and its audit -------------------------------------------------------------

/// Which state of the fixture a frozen result came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The state [`seed`] built: the counted references stand.
    Source,
    /// A copy whose counted references were removed through the chat tools.
    Repaired,
}

/// A refused tool call: code, message and fix.
pub type Refusal = (String, String, String);

/// What [`drive`] saw and wrote, held as plain data.
#[derive(Clone, Debug)]
pub struct Frozen {
    pub profile: String,
    pub scope: String,
    /// The reply of `rdy_publication_check`.
    pub check: Value,
    /// The refusals of the output tools, by name, when the check is blocked.
    pub refused: BTreeMap<String, Refusal>,
    /// What each output wrote (empty when it was refused): `site`, `md`, `mkdocs`, `html`, `pdf`.
    pub outputs: BTreeMap<String, Files>,
    /// The paths each refused output would have written, found on disk afterwards.
    pub leftovers: Vec<String>,
}

fn refusal(r: Reply) -> Result<Value, Refusal> {
    match r {
        Reply::Ok(v) => Ok(v),
        Reply::ToolError(e) => {
            let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
            Err((s("code"), s("message"), s("fix")))
        }
        other => panic!("expected a result or a tool error, got {other:?}"),
    }
}

/// Runs the publication check and every local output of `profile` at `scope` through the live
/// MCP client `c` (a writer session on a served project), writing under `out`. The outputs
/// are: a static site, a Markdown tree (plain and MkDocs), one HTML file and one PDF. A tool
/// that refuses writes nothing, and its refusal is frozen; nothing is retried or waived.
pub fn drive(c: &mut McpClient, profile: &str, scope: &str, out: &Path) -> Frozen {
    let check = c
        .call(
            "rdy_publication_check",
            json!({"profile": profile, "scope": scope}),
        )
        .ok();
    let p = |name: &str| out.join(name);
    let s = |path: &Path| path.to_str().unwrap().to_string();
    let jobs: [(&str, &str, Value); 5] = [
        (
            "site",
            "rdy_render",
            json!({"out": s(&p("site")), "profile": profile, "scope": scope}),
        ),
        (
            "md",
            "rdy_export",
            json!({"out": s(&p("md")), "format": "md", "profile": profile, "scope": scope}),
        ),
        (
            "mkdocs",
            "rdy_export",
            json!({"out": s(&p("mkdocs")), "format": "md", "layout": "mkdocs", "profile": profile, "scope": scope}),
        ),
        (
            "html",
            "rdy_export",
            json!({"out": s(&p("out.html")), "format": "html", "profile": profile, "scope": scope}),
        ),
        (
            "pdf",
            "rdy_export",
            json!({"out": s(&p("out.pdf")), "format": "pdf", "profile": profile, "scope": scope}),
        ),
    ];
    let mut frozen = Frozen {
        profile: profile.into(),
        scope: scope.into(),
        check,
        refused: BTreeMap::new(),
        outputs: BTreeMap::new(),
        leftovers: vec![],
    };
    for (name, tool, args) in jobs {
        let at = Path::new(args["out"].as_str().unwrap()).to_path_buf();
        match refusal(c.call(tool, args)) {
            Ok(_) => {
                frozen.outputs.insert(name.into(), tree(&at));
            }
            Err(e) => {
                frozen.refused.insert(name.into(), e);
                if at.exists() {
                    frozen.leftovers.push(name.into());
                }
            }
        }
    }
    frozen
}

/// A fresh short scratch directory for [`drive`]'s outputs.
pub fn scratch() -> tempfile::TempDir {
    short_tempdir()
}

/// Reads a frozen result against the fixture's expectations; returns every violation, none when
/// it holds. It reads the frozen check and files only, and calls no engine.
///
/// - The check: the profile's audience sets the status (a public profile with counted references
///   is blocked, an org profile warns, none counted is clear); the counted rows are the fixture's
///   (no row at all once repaired); the hidden link edge is a `links_outside` row, never a row;
///   an outside decision is no row; every selected block is in the selection and no excluded one.
/// - A blocked check refused every output and wrote nothing.
/// - Otherwise every output exists, holds none of the excluded words, names or assets, shows the
///   selected words, reads the decision citations as the author's words and resolves every local
///   link. `print` is the text [`print_text`] returned for the PDF's model.
pub fn audit(f: &Frozen, s: &Scenario, stage: Stage, print: Option<&str>) -> Vec<String> {
    let mut bad: Vec<String> = vec![];
    macro_rules! want {
        ($ok:expr, $what:expr $(,)?) => {
            if !$ok {
                bad.push($what);
            }
        };
    }
    let check = &f.check;
    let public = check["profile"]["audience"] == "public";
    let (excluded, missing) = match stage {
        Stage::Source => s.counted(),
        Stage::Repaired => (0, 0),
    };
    let status = match (excluded + missing, public) {
        (0, _) => "clear",
        (_, true) => "blocked",
        (_, false) => "warning",
    };
    let refs = &check["references"];
    want!(
        refs["status"] == status,
        format!("status {} is not {status}", refs["status"]),
    );
    want!(
        (refs["excluded"].as_u64(), refs["missing"].as_u64()) == (Some(excluded), Some(missing)),
        format!(
            "counts {}/{} are not {excluded}/{missing}",
            refs["excluded"], refs["missing"]
        ),
    );
    let mut rows: Vec<Value> = refs["rows"].as_array().cloned().unwrap_or_default();
    rows.sort_by_key(Value::to_string);
    let mut expect: Vec<Value> = match stage {
        Stage::Source => s.rows.iter().map(Row::json).collect(),
        Stage::Repaired => vec![],
    };
    expect.sort_by_key(Value::to_string);
    want!(
        rows == expect,
        format!("rows {rows:?} are not the fixture's {expect:?}"),
    );
    let mut links: Vec<Value> = refs["links_outside"]["rows"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    links.sort_by_key(Value::to_string);
    let expect_links: Vec<Value> = s.link_rows.iter().map(Row::json).collect();
    want!(
        links == expect_links,
        format!("links outside {links:?} are not {expect_links:?}"),
    );
    for d in &s.dropped_decisions {
        let named = rows
            .iter()
            .chain(&links)
            .any(|r| r["target"] == d.as_str() || r["source"] == d.as_str());
        want!(!named, format!("the outside decision {d} is counted"));
    }
    let selected: BTreeSet<String> = check["selection"]["entries"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| e["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for id in &s.selected {
        want!(selected.contains(id), format!("{id} is not selected"));
    }
    for id in &s.excluded {
        want!(!selected.contains(id), format!("{id} is selected"));
    }

    if status == "blocked" {
        for name in ["site", "md", "mkdocs", "html", "pdf"] {
            match f.refused.get(name) {
                Some((code, ..)) if code == "E_PUBLICATION_REFERENCE" => {}
                other => bad.push(format!("{name} was not refused by the check: {other:?}")),
            }
        }
        want!(f.outputs.is_empty(), "a refused output wrote files".into());
        want!(
            f.leftovers.is_empty(),
            format!("a refusal left {:?}", f.leftovers),
        );
        return bad;
    }
    want!(
        f.refused.is_empty(),
        format!("an output was refused: {:?}", f.refused),
    );
    let mut all = Files::new();
    for (name, files) in &f.outputs {
        want!(!files.is_empty(), format!("{name} wrote nothing"));
        for (path, bytes) in files {
            all.insert(format!("{name}/{path}"), bytes.clone());
        }
        let own: Files = files.clone();
        for word in &s.excluded_words {
            let hit = holding(&own, word.as_bytes());
            want!(hit.is_empty(), format!("{name}: {word} is in {hit:?}"));
        }
        let bytes = &s.excluded_png;
        let there =
            !holding(&own, bytes).is_empty() || !holding(&own, base64(bytes).as_bytes()).is_empty();
        want!(!there, format!("{name} holds the excluded figure"));
        // no local reference reaches the excluded doc, its anchors or a missing page
        let scan = scan_local_links(files);
        want!(
            scan.broken.is_empty(),
            format!("{name}: broken local links {:?}", scan.broken),
        );
    }
    // the selected words and the citation words read in the outputs a person opens
    for name in ["site", "md", "html"] {
        let Some(files) = f.outputs.get(name) else {
            bad.push(format!("{name} is not among the outputs"));
            continue;
        };
        let org = check["profile"]["audience"] == "org";
        for word in &s.selected_words {
            // the description is the profile's; a public profile's differs
            if word.starts_with("Description of") && !org {
                continue;
            }
            if holding(files, word.as_bytes()).is_empty() {
                bad.push(format!("{name} lacks {word}"));
            }
        }
        if name != "html" {
            let flat: String = files
                .values()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .collect::<Vec<_>>()
                .join("\n");
            if !flat.contains(&s.citation_words) && name == "md" {
                bad.push(format!("{name} lacks the citation words"));
            }
        }
    }
    // the surfaces of a site that a profile filters: search, the llms files, the trail and versions
    if let Some(site) = f.outputs.get("site") {
        let text = |path: &str| {
            site.get(path)
                .map(|b| String::from_utf8_lossy(b).into_owned())
        };
        for (path, word) in [
            ("search-index.json", "ALPHAKEEP"),
            ("llms.txt", "Alpha guide"),
            ("llms-full.txt", "BRAVOKEEP"),
        ] {
            match text(path) {
                Some(t) => want!(t.contains(word), format!("{path} lacks {word}")),
                None => bad.push(format!("the site has no {path}")),
            }
        }
        let alpha = text("alpha.html").unwrap_or_default();
        let org = check["profile"]["audience"] == "org";
        let trail = alpha.find("data-rdy=\"trail\"");
        match (org, trail) {
            (true, Some(at)) => want!(
                alpha[at..].contains("SELDECCANARY ruling."),
                "the trail lacks the governing decision".to_string()
            ),
            (true, None) => bad.push("the org trail is missing".into()),
            (false, Some(_)) => bad.push("a public profile shows a trail".into()),
            (false, None) => {}
        }
        for (path, bytes) in site {
            if path.ends_with(".html") {
                let page = String::from_utf8_lossy(bytes);
                want!(
                    !page.contains("id=\"versions\"") && !page.contains("#versions"),
                    format!("{path} shows source versions")
                );
            }
        }
    }
    if let Some(print) = print {
        for word in &s.excluded_words {
            if print.contains(word.as_str()) {
                bad.push(format!("the print model or PDF text holds {word}"));
            }
        }
        for word in ["ALPHAKEEP", "BRAVOKEEP", &s.citation_words] {
            if !print.contains(word) {
                bad.push(format!("the PDF text lacks {word}"));
            }
        }
    }
    bad
}

// --- the repair ----------------------------------------------------------------------------------

/// What [`repair_through_mcp`] reached.
#[derive(Clone, Debug)]
pub struct Repair {
    /// The check of `pubp` in the repaired view, before it landed: the ready selection.
    pub ready: Value,
    /// The check of `pubp` in canon after the person's relayed answer landed the repair.
    pub landed: Value,
    /// The landing that holds the repaired selection (render and scan `landing:<id>`).
    pub landing: String,
}

/// The answer words of the simulated person, as the harness relays them.
fn person(extra: Value) -> Value {
    let mut v = json!({
        "evidence": "Yes, remove the references that leave the selection.",
        "question": "Shall I remove the references that leave the public selection?",
        "session_ref": "chat-mg6"
    });
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

/// Repairs every counted reference of the fixture through the chat tools of the writer session
/// `c`: it removes each reference (it adds no profile and no waiver), checks the repaired view,
/// holds that ready selection, then proposes the repair and lands it with the person's relayed
/// answers. `view` is the view the writer session holds. The source-only link and the outside decision citations stay as they were. Panics on
/// a refusal, so a repair that did not happen cannot pass for one.
pub fn repair_through_mcp(c: &mut McpClient, s: &Scenario, view: &str) -> Repair {
    let id = |k: &str| s.id(k).to_string();
    c.call(
        "rdy_write",
        json!({"ops": [
            {"op": "update", "id": id("$sA"), "fields": {"title": "Alpha section"}},
            {"op": "update", "id": id("$pA2"), "body": "Explicit Shown display words and bare words and typed words."},
            {"op": "update", "id": id("$tA"), "fields": {"columns": ["Label"], "rows": [["Cell"]]}},
            {"op": "update", "id": id("$fA"), "fields": {"alt": "Selected figure alt"}},
            {"op": "update", "id": id("$pM"), "body": "Missing words here."}
        ]}),
    )
    .ok();
    let ready = c
        .call(
            "rdy_publication_check",
            json!({"profile": "pubp", "scope": format!("view:{view}")}),
        )
        .ok();
    let p = c
        .call(
            "rdy_propose",
            json!({"title": "Remove references that leave the selection",
                   "summary": "Removes the references from the public selection to blocks outside it."}),
        )
        .ok();
    let pref = p["ref"].as_str().expect("the proposal ref").to_string();
    for _ in 0..12 {
        let queue = c.call("rdy_pending", json!({})).ok();
        let Some(item) = queue["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["ref"] == pref.as_str() && i["kind"] != "approve")
            .cloned()
        else {
            break;
        };
        c.call(
            "rdy_answer",
            person(
                json!({"ref": item["ref"], "option": "confirm", "card_hash": item["card_hash"]}),
            ),
        )
        .ok();
    }
    let queue = c.call("rdy_pending", json!({})).ok();
    let card = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == pref.as_str() && i["kind"] == "approve")
        .unwrap_or_else(|| panic!("the approval card for {pref} stands: {queue:#}"))
        .clone();
    c.call(
        "rdy_answer",
        person(json!({
            "ref": pref,
            "option": card["options"][0]["id"],
            "card_hash": card["card_hash"],
        })),
    )
    .ok();
    let landed = c
        .call("rdy_publication_check", json!({"profile": "pubp"}))
        .ok();
    let landing = landed["scope"]["landing"]
        .as_str()
        .expect("canon has a landing")
        .to_string();
    Repair {
        ready,
        landed,
        landing,
    }
}
