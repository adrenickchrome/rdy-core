//! The print as one HTML file (D-6SANB, D-X61DT): it opens offline and nothing in it acts.
//!
//! The file holds a `<style>` (the theme's `print.css` and its two fonts as `data:` URIs), the
//! title part, a contents list, then one `<section>` per print doc. Every figure is an `<img>`
//! with a `data:` URI. There is no script, link element, form or button, and no URL but an
//! author's external link (FW-RV3a).
//!
//! **Ids (O31).** A page's id is its path without `.html`, with `/` kept (`wallets/tx`). A
//! block's anchor becomes `<page id>:<anchor>`. Names and anchors use only `A-Z a-z 0-9 . _ -`,
//! so no two ids clash. An internal link is `#<page id>` or `#<page id>:<anchor>`. No block id
//! is in the file (O4).
use super::{Href, PBlock, PageKind, Print, PrintDoc, PrintSummary, Run};
use rdy_engine::{E_BAD_INPUT, E_IO, RdyError, dump::place_file};
use std::fmt::Write as _;
use std::path::Path;

/// The sheet of the file, after the fonts.
const PRINT_CSS: &str = include_str!("../../theme/rdy-standard/print.css");

/// Writes the print as one HTML file at `out`. O29's path rules come first: the directory of
/// `out` must exist and must not be the project's `.rdy` directory or inside it. `out` must not
/// exist; the file is placed whole or not at all (`rdy_engine::dump::place_file`).
pub fn write_single_html(p: &Print, out: &Path) -> Result<PrintSummary, RdyError> {
    check_path(out)?;
    let bytes = single_html(p);
    let n = place_file(out, &bytes, "rdy export")?;
    let count = |k: PageKind| p.docs.iter().filter(|d| d.kind == k).count();
    Ok(PrintSummary {
        out: out.display().to_string(),
        format: "html",
        docs: count(PageKind::Doc),
        folders: count(PageKind::Folder),
        assets: p.blobs.len(),
        pages: None,
        bytes: n,
        warnings: p.warnings.clone(),
    })
}

/// O29's path rules for a file: the parent of `out` exists (`E_BAD_INPUT`), and neither it nor
/// `out` is in a `.rdy` directory. A file in `.rdy` can break the store. The check reads the
/// resolved path, so a symlink or `..` does not pass.
fn check_path(out: &Path) -> Result<(), RdyError> {
    let dir = out
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !dir.exists() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{} does not exist", dir.display()),
            "create the directory first, or pick another path",
        ));
    }
    let resolved = std::fs::canonicalize(dir)
        .map_err(|e| RdyError::new(E_IO, format!("{}: {e}", dir.display()), "check the path"))?;
    let in_rdy = |s: &std::ffi::OsStr| s == ".rdy";
    if resolved.iter().any(in_rdy) || out.file_name().is_some_and(in_rdy) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{} is inside the project .rdy directory", out.display()),
            "export to a path outside .rdy",
        ));
    }
    Ok(())
}

/// The bytes of the file. The same print gives the same bytes.
pub fn single_html(p: &Print) -> Vec<u8> {
    let mut h = String::new();
    h.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    h.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    let _ = writeln!(h, "<title>{}</title>", esc(&p.title));
    h.push_str("<style>\n");
    for (family, file) in [
        ("Onest", "assets/fonts/onest.woff2"),
        ("Geist Mono", "assets/fonts/geist-mono.woff2"),
    ] {
        let bytes = crate::assets::THEME
            .iter()
            .find(|(name, _)| *name == file)
            .map_or(&[][..], |(_, b)| *b);
        let _ = writeln!(
            h,
            "@font-face {{ font-family: '{family}'; src: url(data:font/woff2;base64,{}) format('woff2'); font-weight: 100 900; font-style: normal; font-display: swap; }}",
            base64(bytes)
        );
    }
    h.push_str(PRINT_CSS);
    if !PRINT_CSS.ends_with('\n') {
        h.push('\n');
    }
    h.push_str("</style>\n</head>\n<body>\n");

    // the title part
    h.push_str("<header class=\"title-part\">\n");
    let _ = writeln!(h, "<h1>{}</h1>", esc(&p.title));
    if let Some(d) = p.description.as_deref().filter(|d| !d.trim().is_empty()) {
        let _ = writeln!(h, "<p class=\"description\">{}</p>", esc(d));
    }
    let _ = writeln!(
        h,
        "<p class=\"colophon\">{} <span class=\"scope\">({})</span></p>",
        esc(&p.label),
        esc(&p.scope)
    );
    h.push_str("</header>\n");

    contents(&mut h, &p.docs);

    for d in &p.docs {
        section(&mut h, p, d);
    }
    h.push_str("</body>\n</html>\n");
    h.into_bytes()
}

/// A page's id: its path without `.html`.
fn page_id(page: &str) -> &str {
    page.strip_suffix(".html").unwrap_or(page)
}

/// The id of a block's anchor on a page.
fn anchor_id(page: &str, anchor: &str) -> String {
    format!("{}:{anchor}", page_id(page))
}

/// The contents list: nested `<ul>`s by depth, one `<li><a>` per print doc in tree order.
fn contents(h: &mut String, docs: &[PrintDoc]) {
    h.push_str("<nav class=\"contents\">\n<h2>Contents</h2>\n");
    // the number of open lists; an item's `<li>` stays open until the next item shows whether
    // a list nests inside it
    let mut open = 0usize;
    for d in docs {
        let want = (d.depth + 1).clamp(1, open + 1);
        if open == 0 {
            h.push_str("<ul>\n");
            open = 1;
        } else if want > open {
            h.push_str("\n<ul>\n");
            open += 1;
        } else {
            h.push_str("</li>\n");
            while open > want {
                h.push_str("</ul>\n</li>\n");
                open -= 1;
            }
        }
        let _ = write!(
            h,
            "<li><a href=\"#{}\">{}</a>",
            esc(page_id(&d.page)),
            esc(&d.title)
        );
    }
    if open > 0 {
        h.push_str("</li>\n");
        while open > 1 {
            h.push_str("</ul>\n</li>\n");
            open -= 1;
        }
        h.push_str("</ul>\n");
    }
    h.push_str("</nav>\n");
}

fn section(h: &mut String, p: &Print, d: &PrintDoc) {
    let class = match d.kind {
        PageKind::Folder => "folder",
        PageKind::Doc => "doc",
        PageKind::Glossary => "glossary",
    };
    let _ = writeln!(
        h,
        "<section id=\"{}\" class=\"{class}\">\n<h2 class=\"doc-title\">{}</h2>",
        esc(page_id(&d.page)),
        esc(&d.title)
    );
    blocks(h, p, &d.page, &d.blocks);
    h.push_str("</section>\n");
}

/// The attribute that gives a block its id, with a leading space; empty without an anchor.
fn id_attr(page: &str, anchor: &Option<String>) -> String {
    anchor.as_deref().map_or_else(String::new, |a| {
        format!(" id=\"{}\"", esc(&anchor_id(page, a)))
    })
}

fn blocks(h: &mut String, p: &Print, page: &str, list: &[PBlock]) {
    for b in list {
        block(h, p, page, b);
    }
}

fn block(h: &mut String, p: &Print, page: &str, b: &PBlock) {
    match b {
        PBlock::Heading {
            level,
            anchor,
            runs: r,
        } => {
            // the doc's title is the section's `h2`: a heading sits one level below its own
            let n = (level + 1).clamp(3, 6);
            let _ = writeln!(h, "<h{n}{}>{}</h{n}>", id_attr(page, anchor), runs(r));
        }
        PBlock::Para { anchor, runs: r } => {
            let _ = writeln!(h, "<p{}>{}</p>", id_attr(page, anchor), runs(r));
        }
        PBlock::List {
            ordered,
            start,
            items,
        } => {
            let tag = if *ordered { "ol" } else { "ul" };
            if *ordered && *start != 1 {
                let _ = writeln!(h, "<ol start=\"{start}\">");
            } else {
                let _ = writeln!(h, "<{tag}>");
            }
            for item in items {
                h.push_str("<li>");
                blocks(h, p, page, item);
                h.push_str("</li>\n");
            }
            let _ = writeln!(h, "</{tag}>");
        }
        PBlock::Table { anchor, head, rows } => {
            let _ = writeln!(h, "<table{}>", id_attr(page, anchor));
            if !head.is_empty() {
                h.push_str("<thead><tr>");
                for c in head {
                    let _ = write!(h, "<th>{}</th>", runs(c));
                }
                h.push_str("</tr></thead>\n");
            }
            h.push_str("<tbody>\n");
            for row in rows {
                h.push_str("<tr>");
                for c in row {
                    let _ = write!(h, "<td>{}</td>", runs(c));
                }
                h.push_str("</tr>\n");
            }
            h.push_str("</tbody>\n</table>\n");
        }
        PBlock::Code { anchor, lang, text } => {
            let class = lang
                .as_deref()
                .map(|l| {
                    l.chars()
                        .filter(|c| c.is_ascii_alphanumeric() || "_+-".contains(*c))
                        .collect::<String>()
                })
                .filter(|l| !l.is_empty())
                .map(|l| format!(" class=\"language-{l}\""))
                .unwrap_or_default();
            let _ = writeln!(
                h,
                "<pre{}><code{class}>{}</code></pre>",
                id_attr(page, anchor),
                esc(text)
            );
        }
        PBlock::Figure {
            anchor,
            blob,
            alt,
            caption,
        } => {
            let _ = writeln!(h, "<figure{}>", id_attr(page, anchor));
            let img = blob.and_then(|b| p.blobs.get(&b));
            if let Some(blob) = img {
                let _ = writeln!(
                    h,
                    "<img src=\"data:{};base64,{}\" alt=\"{}\">",
                    esc(&blob.media_type),
                    base64(&blob.bytes),
                    esc(alt)
                );
            }
            if !caption.is_empty() {
                let _ = writeln!(h, "<figcaption>{}</figcaption>", runs(caption));
            } else if img.is_none() && !alt.is_empty() {
                let _ = writeln!(h, "<figcaption>{}</figcaption>", esc(alt));
            }
            h.push_str("</figure>\n");
        }
        PBlock::Callout {
            anchor,
            label,
            blocks: inner,
        } => {
            let _ = writeln!(h, "<aside class=\"callout\"{}>", id_attr(page, anchor));
            if !label.is_empty() {
                let _ = writeln!(h, "<p class=\"marker\">{}</p>", esc(label));
            }
            blocks(h, p, page, inner);
            h.push_str("</aside>\n");
        }
    }
}

/// The HTML of a line of runs.
fn runs(list: &[Run]) -> String {
    let mut out = String::new();
    for r in list {
        let mut s = esc(&r.text).replace('\n', "<br>");
        if r.code {
            s = format!("<code>{s}</code>");
        }
        if r.italic {
            s = format!("<em>{s}</em>");
        }
        if r.bold {
            s = format!("<strong>{s}</strong>");
        }
        match &r.link {
            Some(Href::Page { page, anchor }) => {
                let to = match anchor {
                    Some(a) => anchor_id(page, a),
                    None => page_id(page).to_string(),
                };
                s = format!("<a href=\"#{}\">{s}</a>", esc(&to));
            }
            Some(Href::External(url)) => s = format!("<a href=\"{}\">{s}</a>", esc(url)),
            None => {}
        }
        out.push_str(&s);
    }
    out
}

/// Text for an element's content or a quoted attribute.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// Standard base64 with padding (RFC 4648).
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn ec7_base64_is_rfc_4648() {
        for (raw, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(raw.as_bytes()), want);
        }
    }
}
