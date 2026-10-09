//! `llms.txt`: the site's contents for an outside agent, in exactly the words a reader sees (FW-D5
//! refined). No id, type, status, hash or JSON appears here.
//!
//! `llms-full.txt` beside it holds every folder's and doc's own text, in reading order, each
//! page's title a `#` heading: the words its page shows, and nothing a reader can't see.
use crate::page::PageContext;
use crate::site::Site;
use crate::version;
use rdy_engine::{E_IO, RdyError};
use std::path::Path;

/// The most characters of a page's first prose that `llms.txt` shows beside its link.
const BLURB_MAX: usize = 160;

/// A text as one line: runs of whitespace become one space.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The five entities `escape` writes, decoded (`&amp;` last, so `&amp;lt;` stays `&lt;`).
fn decode(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// The elements that part words: every other tag (a link, `<code>`, `<strong>`) sits inside a
/// line of text, so `<code>go north</code>;` reads `go north;`.
const BLOCK: [&str; 30] = [
    "p",
    "div",
    "br",
    "hr",
    "li",
    "ul",
    "ol",
    "dl",
    "dt",
    "dd",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "table",
    "thead",
    "tbody",
    "tr",
    "td",
    "th",
    "caption",
    "figure",
    "figcaption",
    "pre",
    "blockquote",
    "aside",
    "details",
    "summary",
];

/// The text of a piece of the renderer's own HTML (the escaped text `text_html` writes): tags
/// dropped (a block-level one parts the words around it), the five entities `escape` writes
/// decoded.
fn plain(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut tag: Option<String> = None;
    for c in html.chars() {
        match (&mut tag, c) {
            // `escape` turns every `<` of the author's text into `&lt;`, so a bare one opens a tag
            (None, '<') => tag = Some(String::new()),
            (Some(t), '>') => {
                let name = t
                    .trim_start_matches('/')
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if BLOCK.contains(&name.as_str()) {
                    text.push(' ');
                }
                tag = None;
            }
            (Some(t), c) => t.push(c),
            (None, c) => text.push(c),
        }
    }
    one_line(&decode(&text))
}

/// `s` as Markdown text: every character that could start markup is backslash-escaped, so the
/// words read as they are written and never become a link, code, emphasis or a heading.
fn md_text(s: &str) -> String {
    md_escape(s, true)
}

/// [`md_text`]; `line_start` says whether `s` begins a line, where a `#`, `-`, `+` or `N.` would
/// open a heading, a list item or a numbered one.
fn md_escape(s: &str, line_start: bool) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let lead = s.len() - s.trim_start().len();
    out.push_str(&s[..lead]);
    let body = &s[lead..];
    if line_start {
        let digits = body.bytes().take_while(u8::is_ascii_digit).count();
        match body.chars().next() {
            Some(c @ ('#' | '-' | '+')) => {
                out.push('\\');
                out.push(c);
                return out + &md_chars(&body[1..]);
            }
            Some(_) if digits > 0 && body[digits..].starts_with(['.', ')']) => {
                out.push_str(&body[..digits]);
                out.push('\\');
                return out + &md_chars(&body[digits..]);
            }
            _ => {}
        }
    }
    out + &md_chars(body)
}

/// `s` with each of `\ ` * _ [ ] < > !` backslash-escaped.
fn md_chars(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '!') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A title as link text: nothing in it can close the link or start a destination.
fn link_text(title: &str) -> String {
    let spaced: String = title
        .chars()
        .map(|c| match c {
            '[' | ']' | '(' | ')' | '\n' | '\r' => ' ',
            c => c,
        })
        .collect();
    spaced.trim().to_string()
}

/// What a page is about, in a line: a folder's description, a doc's first prose.
fn blurb(p: &PageContext, site: &Site) -> Option<String> {
    let text = match p.kind {
        "folder" => site.description_words(p.body.folder.as_ref()?.description.as_deref()?),
        "doc" => {
            // the doc's first prose is its lede when it opens the doc, else the first prose block
            let html = p.body.doc.as_ref()?.lede.as_deref().or_else(|| {
                p.items
                    .iter()
                    .find(|i| i.block_type == "prose")
                    .map(|i| i.html.as_str())
            })?;
            plain(html)
        }
        _ => return None,
    };
    let cut: String = text.chars().take(BLURB_MAX).collect();
    let cut = cut.trim_end();
    (!cut.is_empty()).then(|| md_text(cut))
}

/// What both files open with, as the home page shows it: the site's name, its description when
/// the project has one, and the colophon every page's footer carries.
fn head(site_name: &str, description: Option<&str>, version: &version::Label) -> String {
    let name = one_line(site_name);
    let mut t = format!("# {name}\n\n");
    if let Some(d) = description.map(one_line).filter(|d| !d.is_empty()) {
        t.push_str(&format!("> {d}\n\n"));
    }
    let at = one_line(&version.text);
    if version.frozen {
        t.push_str(&format!("A snapshot of {name} at {at}.\n\n"));
    } else {
        t.push_str(&format!("{name}, {at}.\n\n"));
    }
    t
}

/// The text of `llms.txt`: `pages` are in tree order.
fn index_text(
    site: &Site,
    pages: &[PageContext],
    site_name: &str,
    description: Option<&str>,
    version: &version::Label,
) -> String {
    let mut t = head(site_name, description, version);
    t.push_str("## Pages\n\n");
    for p in pages.iter().filter(|p| matches!(p.kind, "doc" | "folder")) {
        // a folder's page is `<folder>/index.html`, one level above the pages it holds
        let depth = p.page_url.matches('/').count() - usize::from(p.kind == "folder");
        t.push_str(&"  ".repeat(depth));
        t.push_str(&format!(
            "- [{}]({})",
            link_text(&one_line(&p.title)),
            p.page_url
        ));
        if let Some(b) = blurb(p, site) {
            t.push_str(": ");
            t.push_str(&b);
        }
        t.push('\n');
    }
    let reference: Vec<String> = pages
        .iter()
        .filter_map(|p| match p.kind {
            "glossary" => Some(format!("- [Glossary]({})\n", p.page_url)),
            "questions" => Some(format!("- [Open questions]({})\n", p.page_url)),
            _ => None,
        })
        .collect();
    if !reference.is_empty() {
        t.push_str("\n## Reference\n\n");
        t.extend(reference);
    }
    t
}

/// How a line of [`reader_text`] joins the one before it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line {
    Paragraph,
    /// A list item: consecutive ones are not parted by a blank line.
    Item,
    /// A table row: likewise.
    Row,
    /// A fenced block of preformatted text.
    Fence,
}

/// The lines [`reader_text`] builds.
#[derive(Default)]
struct Lines {
    done: Vec<(Line, String)>,
    /// The text of the line being built, its whitespace collapsed when it ends.
    cur: String,
    /// What the line being built starts with (a heading's `#`s, a list item's marker).
    prefix: String,
    kind: Option<Line>,
    /// The open lists: a counter for a numbered one.
    lists: Vec<Option<usize>>,
    /// The cells of the table row being built.
    cells: usize,
}

impl Lines {
    /// Ends the line being built; a line with no words is dropped.
    fn flush(&mut self) {
        let words = one_line(&self.cur);
        self.cur.clear();
        if words.is_empty() {
            return;
        }
        let kind = self.kind.take().unwrap_or(Line::Paragraph);
        let line = std::mem::take(&mut self.prefix) + &words;
        self.done.push((kind, line));
    }

    /// Ends the line being built, and what it started with even when it had no words: an empty
    /// list item or heading does not lend its marker to the next line.
    fn end(&mut self) {
        self.flush();
        self.kind = None;
        self.prefix.clear();
    }

    /// Starts a line of `kind` that begins with `prefix`.
    fn start(&mut self, kind: Line, prefix: String) {
        self.flush();
        self.kind = Some(kind);
        self.prefix = prefix;
    }

    /// The lines, a blank line between two that are not items of one list or rows of one table.
    fn text(&self) -> String {
        let mut out = String::new();
        let mut last: Option<Line> = None;
        for (kind, line) in &self.done {
            if let Some(l) = last {
                let tight = l == *kind && matches!(kind, Line::Item | Line::Row);
                out.push_str(if tight { "\n" } else { "\n\n" });
            }
            out.push_str(line);
            last = Some(*kind);
        }
        out
    }
}

/// The value of attribute `key` in a start tag's text (its name, then its attributes), as written:
/// quoted values may hold spaces and `=`, so a title or href that mentions an attribute is not one.
fn attr<'a>(tag: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = tag.trim_start_matches(|c: char| !c.is_whitespace());
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        if rest.is_empty() {
            return None;
        }
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '=' || c == '/')
            .unwrap_or(rest.len());
        let name = &rest[..end];
        rest = rest[end..].trim_start();
        let value = match rest.strip_prefix('=') {
            Some(v) => {
                let v = v.trim_start();
                let (value, after) = match v.chars().next() {
                    Some(q @ ('"' | '\'')) => {
                        let body = &v[1..];
                        let close = body.find(q).unwrap_or(body.len());
                        (&body[..close], body.get(close + 1..).unwrap_or_default())
                    }
                    _ => {
                        let close = v.find(char::is_whitespace).unwrap_or(v.len());
                        (&v[..close], &v[close..])
                    }
                };
                rest = after;
                value
            }
            None => "",
        };
        if name.eq_ignore_ascii_case(key) {
            return Some(value);
        }
    }
}

/// An element whose words a reader never sees on the page: a button's label, what only a screen
/// reader hears (`class="visually-hidden"`), the diagram canvas (`aria-hidden="true"`; its
/// words are under "Diagram source"), and a script, a stylesheet (an SVG's own included) or a
/// template. Only the attributes count, never an href or title that mentions them.
fn unseen(name: &str, tag: &str) -> bool {
    matches!(name, "button" | "script" | "style" | "template")
        || attr(tag, "aria-hidden") == Some("true")
        || attr(tag, "class").is_some_and(|c| c.split_whitespace().any(|c| c == "visually-hidden"))
}

/// The elements that have no closing tag.
const VOID: [&str; 6] = ["br", "img", "hr", "input", "meta", "link"];

/// The words a reader reads in a piece of the renderer's own HTML, as Markdown: a heading's level
/// as `#`s, list items as `- ` or `1. `, a table's rows as `|` cells and preformatted text
/// fenced. Attributes, buttons, scripts, styles and what only a screen reader hears are left out.
fn reader_text(html: &str) -> String {
    let mut out = Lines::default();
    // the element being skipped, and how many of its kind are open inside the walk
    let mut skip: Option<(String, usize)> = None;
    let mut pre = false;
    let mut rest = html;
    loop {
        let (text, tag) = match rest.find('<') {
            Some(i) => {
                let close = rest[i..].find('>').map_or(rest.len(), |e| i + e);
                (&rest[..i], Some(&rest[i + 1..close]))
            }
            None => (rest, None),
        };
        if skip.is_none() {
            let text = decode(text);
            if pre {
                out.cur.push_str(&text);
            } else {
                // a `|` in a cell would end the cell
                let line_start = out.cur.trim().is_empty() && out.prefix.is_empty();
                let text = md_escape(&text, line_start);
                if out.kind == Some(Line::Row) {
                    out.cur.push_str(&text.replace('|', "\\|"));
                } else {
                    out.cur.push_str(&text);
                }
            }
        }
        let Some(tag) = tag else { break };
        rest = rest.get(text.len() + tag.len() + 2..).unwrap_or_default();
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if let Some((skipped, depth)) = &mut skip {
            if *skipped == name {
                if !closing {
                    *depth += 1;
                } else if *depth == 1 {
                    skip = None;
                } else {
                    *depth -= 1;
                }
            }
            continue;
        }
        if !closing && unseen(&name, tag) && !VOID.contains(&name.as_str()) {
            skip = Some((name, 1));
            continue;
        }
        if pre {
            // the highlighter's spans are inline; the text keeps its own lines
            if closing && name == "pre" {
                let code = std::mem::take(&mut out.cur);
                // a fence longer than any run of backticks in the code cannot be closed by it
                let longest = code.split(|c| c != '`').map(str::len).max().unwrap_or(0);
                let fence = "`".repeat(longest.saturating_add(1).max(3));
                out.done.push((
                    Line::Fence,
                    format!("{fence}\n{}\n{fence}", code.trim_end_matches('\n')),
                ));
                pre = false;
            }
            continue;
        }
        match (name.as_str(), closing) {
            ("pre", false) => {
                out.flush();
                pre = true;
            }
            ("ul", false) => out.lists.push(None),
            ("ol", false) => out.lists.push(Some(
                attr(tag, "start").and_then(|s| s.parse().ok()).unwrap_or(1),
            )),
            ("ul" | "ol", true) => {
                out.end();
                out.lists.pop();
            }
            ("li", false) => {
                let indent = "  ".repeat(out.lists.len().saturating_sub(1));
                let marker = match out.lists.last_mut() {
                    Some(Some(n)) => {
                        let marker = format!("{n}. ");
                        *n += 1;
                        marker
                    }
                    _ => "- ".to_string(),
                };
                out.start(Line::Item, indent + &marker);
            }
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) => {
                let level = usize::from(name.as_bytes()[1] - b'0');
                out.start(Line::Paragraph, "#".repeat(level) + " ");
            }
            ("tr", false) => {
                out.start(Line::Row, String::new());
                out.cells = 0;
            }
            ("td" | "th", false) => {
                out.cur.push_str(if out.cells == 0 { "| " } else { " | " });
                out.cells += 1;
            }
            ("tr", true) => {
                out.cur.push_str(" |");
                out.end();
            }
            ("thead", true) => {
                out.end();
                let rule = format!("|{}", " --- |".repeat(out.cells.max(1)));
                out.done.push((Line::Row, rule));
            }
            ("li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6", true) => out.end(),
            (
                "p" | "div" | "dl" | "dt" | "dd" | "table" | "caption" | "figure" | "figcaption"
                | "aside" | "details" | "summary" | "blockquote",
                _,
            ) => out.flush(),
            // a line break keeps its row, item or paragraph: it reads as a space
            ("br", _) => out.cur.push(' '),
            _ => {}
        }
    }
    out.flush();
    out.text()
}

/// The text of `llms-full.txt`: every folder's and doc's own words, `pages` in tree order.
fn full_text(
    pages: &[PageContext],
    site_name: &str,
    description: Option<&str>,
    version: &version::Label,
) -> String {
    let mut t = head(site_name, description, version);
    for p in pages.iter().filter(|p| matches!(p.kind, "doc" | "folder")) {
        t.push_str(&format!("# {}\n\n", md_text(&one_line(&p.title))));
        let mut parts: Vec<String> = vec![];
        if let Some(f) = &p.body.folder {
            parts.extend(f.description_html.as_deref().map(|d| format!("<p>{d}</p>")));
        }
        if let Some(d) = &p.body.doc {
            parts.extend(d.lede.as_deref().map(|l| format!("<p>{l}</p>")));
        }
        parts.extend(p.items.iter().map(|i| i.html.clone()));
        for part in parts {
            let text = reader_text(&part);
            if !text.is_empty() {
                t.push_str(&text);
                t.push_str("\n\n");
            }
        }
    }
    t.truncate(t.trim_end().len());
    t.push('\n');
    t
}

fn write(out: &Path, file: &str, text: String) -> Result<(), RdyError> {
    std::fs::write(out.join(file), text).map_err(|e| {
        RdyError::new(
            E_IO,
            format!("cannot write {file}: {e}"),
            "check the output directory",
        )
    })
}

/// Writes `llms.txt` under `out`.
pub fn write_index(
    site: &Site,
    out: &Path,
    pages: &[PageContext],
    site_name: &str,
    description: Option<&str>,
    version: &version::Label,
) -> Result<(), RdyError> {
    write(
        out,
        "llms.txt",
        index_text(site, pages, site_name, description, version),
    )
}

/// Writes `llms-full.txt` under `out`.
pub fn write_full(
    out: &Path,
    pages: &[PageContext],
    site_name: &str,
    description: Option<&str>,
    version: &version::Label,
) -> Result<(), RdyError> {
    write(
        out,
        "llms-full.txt",
        full_text(pages, site_name, description, version),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_drops_tags_and_decodes_what_escape_wrote() {
        assert_eq!(
            plain("A <a href=\"x\" title=\"t\">link</a> &amp; &lt;b&gt; it&#39;s<br>fine"),
            "A link & <b> it's fine"
        );
    }

    #[test]
    fn msw12_a_line_break_does_not_end_a_row_an_item_or_a_paragraph() {
        assert_eq!(
            reader_text("<table><tbody><tr><td>one<br>two</td><td>2</td></tr></tbody></table>"),
            "| one two | 2 |"
        );
        assert_eq!(
            reader_text("<ol><li>first<br>still first</li><li>second</li></ol>"),
            "1. first still first\n2. second"
        );
        assert_eq!(reader_text("<p>a<br>b</p>"), "a b");
    }

    #[test]
    fn msw12_a_fence_is_longer_than_any_backtick_run_in_the_code() {
        assert_eq!(
            reader_text("<pre><code>```py\nx = 1\n```</code></pre>"),
            "````\n```py\nx = 1\n```\n````"
        );
    }

    #[test]
    fn msw12_a_numbered_list_keeps_its_start() {
        assert_eq!(
            reader_text("<ol start=\"3\"><li>three</li><li>four</li></ol>"),
            "3. three\n4. four"
        );
    }

    #[test]
    fn llms1_an_inline_tag_is_not_a_word_break() {
        assert_eq!(
            plain(
                "Type <code>get sword</code> or <code>go north</code>; a <a href=\"x\">token</a>, \
                <code>?</code>, <em>stands</em>.<p>Next</p>"
            ),
            "Type get sword or go north; a token, ?, stands. Next"
        );
    }

    #[test]
    fn a_link_text_cannot_close_the_link_or_open_a_destination() {
        assert_eq!(link_text("a](javascript:x)\nb"), "a  javascript:x  b");
    }

    #[test]
    fn llms1_reader_text_keeps_headings_lists_tables_and_code_and_drops_what_no_one_sees() {
        let html = "<h2 class=\"section-title\">The <code>case</code> clause</h2>\
            <p>A <a href=\"x.html\" title=\"Doc\">guard</a> &amp; a <strong>pattern</strong>.</p>\
            <ul><li>one</li><li>two<ol><li>first</li></ol></li></ul>\
            <div class=\"table-wrap\"><table><caption>Table 1</caption><thead><tr><th>A</th>\
            <th>B</th></tr></thead><tbody><tr><td>a | b</td><td>2</td></tr></tbody></table></div><ul><li></li></ul><h3></h3>\
            <div class=\"code-block\"><div class=\"code-head\"><span class=\"lang\">Python</span>\
            <button type=\"button\" class=\"tool copy-code\" aria-label=\"Copy code\">Copy</button>\
            </div><pre class=\"code\"><code><span class=\"k\">if</span> x &lt; 1:\n    pass</code>\
            </pre></div><figure class=\"diagram\"><p class=\"visually-hidden\">Alt</p>\
            <div class=\"diagram-canvas\" aria-hidden=\"true\"><pre class=\"mermaid\">A#91;x#93;\
            </pre></div><figcaption>Alt</figcaption></figure>";
        assert_eq!(
            reader_text(html),
            "## The case clause\n\nA guard & a pattern.\n\n- one\n- two\n  1. first\n\n\
             Table 1\n\n| A | B |\n| --- | --- |\n| a \\| b | 2 |\n\nPython\n\n\
             ```\nif x < 1:\n    pass\n```\n\nAlt"
        );
    }

    #[test]
    fn llms1_reader_text_drops_scripts_styles_and_templates() {
        let html = "<p>Before</p><script>var x = \"<p>no</p>\";</script><style>.a{color:red}</style>\
            <template><p>Later</p></template><figure><svg viewBox=\"0 0 1 1\"><style>text{fill:red}\
            </style><text>Label</text></svg><figcaption>After</figcaption></figure>";
        assert_eq!(reader_text(html), "Before\n\nLabel\n\nAfter");
    }

    #[test]
    fn llms1_a_link_keeps_its_words_whatever_its_href_or_title_says() {
        let html = "<p>Screen readers hear <a href=\"#the-visually-hidden-pattern\" \
            title=\"Accessibility \u{203a} The visually-hidden pattern\">The visually-hidden \
            pattern</a> when the text is off screen; see also <a href=\"https://example.invalid/\
            visually-hidden/\" title=\"x aria-hidden=&quot;true&quot; visually-hidden\">the \
            pattern</a>.</p><p class=\"note visually-hidden\">Gone</p>\
            <span data-x=\"class=visually-hidden\" aria-hidden=\"true\">Gone too</span>";
        assert_eq!(
            reader_text(html),
            "Screen readers hear The visually-hidden pattern when the text is off screen; see \
             also the pattern."
        );
    }
}
