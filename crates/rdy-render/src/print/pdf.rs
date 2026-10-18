//! The print as one PDF file (D-6SANB; RULING F1): written natively in Rust, with no browser.
//!
//! **The file (closed).** PDF 1.7. Each page of the layout ([`layout::lay_out`] on A4, O5) is a
//! page, drawn as the layout places it. Each DejaVu face in use is a subset (`subsetter`),
//! embedded as a CIDFontType2 font with Identity-H and a ToUnicode map; its glyphs are addressed
//! by their ids in the subset. Courier is the standard font, with WinAnsiEncoding. Italic is the
//! Sans subset, and Bold Italic the Bold one, with a 12° text matrix. Content streams, font files,
//! ToUnicode maps and image data are Flate-compressed (`miniz_oxide`, level 6). Each layout
//! outline entry is an outline item, nested by level. A link to a print page is a GoTo to its
//! anchor's page and y (to the doc's first page when the anchor is None); an external link is a
//! URI action. The document info holds `/Producer (rdy <version>)` and `/Title`, and no date, so
//! the same print gives the same bytes (O15).
use super::fonts::{Face, Fonts, SANS, SANS_BOLD};
use super::image::{self, Pixels};
use super::layout::{self, LItem, Laid, OutlineEntry, PageSpec};
use super::{Href, PBlock, PageKind, Print, PrintSummary};
use pdf_writer::types::{
    ActionType, AnnotationType, CidFontType, FontFlags, SystemInfo, UnicodeCmap,
};
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref, Str, TextStr};
use rdy_engine::{RdyError, dump::place_file};
use rdy_model::Hash;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The slant of Italic and Bold Italic, in degrees.
const SLANT: f32 = 12.0;
/// The Flate level of every compressed stream.
const LEVEL: u8 = 6;
/// The resource names of the three fonts.
const SANS_NAME: &[u8] = b"F0";
const BOLD_NAME: &[u8] = b"F1";
const MONO_NAME: &[u8] = b"F2";

/// The layout the PDF draws: the print on A4 pages, with the figures whose images print
/// ([`image::decode`]) at their size and every other figure as a line.
pub fn lay_out(p: &Print, fonts: &Fonts) -> Laid {
    let sizes = p
        .blobs
        .iter()
        .filter_map(|(h, b)| image::decode(&b.media_type, &b.bytes).map(|px| (*h, px.size())))
        .collect();
    lay_out_with(p, fonts, &sizes)
}

/// The PDF of `p`: its bytes, its number of pages and its warnings. The warnings are the build's,
/// then one per figure that does not print (its type or form does not print, or it is over the
/// budget of [`image::decode`]), in print order, then one for the characters with no glyph. The
/// same print gives the same bytes.
pub fn pdf_bytes(p: &Print, fonts: &Fonts) -> (Vec<u8>, usize, Vec<String>) {
    let r = render(p, fonts);
    (r.bytes, r.pages, r.warnings)
}

/// Writes the print as one PDF file at `out`. O29's path rules come first: the directory of `out`
/// must exist and must not be in the project's `.rdy` directory. `out` must not exist; the file is
/// placed whole or not at all (`rdy_engine::dump::place_file`).
pub fn write_pdf(p: &Print, out: &Path) -> Result<PrintSummary, RdyError> {
    super::single::check_path(p, out)?;
    let r = render(p, &Fonts::load());
    let n = place_file(out, &r.bytes, "rdy export")?;
    let count = |k: PageKind| p.docs.iter().filter(|d| d.kind == k).count();
    Ok(PrintSummary {
        out: out.display().to_string(),
        format: "pdf",
        docs: count(PageKind::Doc),
        folders: count(PageKind::Folder),
        assets: r.images,
        pages: Some(r.pages),
        bytes: n,
        warnings: r.warnings,
    })
}

/// A written PDF: its bytes, pages, warnings and the number of images it holds.
struct Rendered {
    bytes: Vec<u8>,
    pages: usize,
    warnings: Vec<String>,
    images: usize,
}

/// The layout with the figure images of these sizes; every other figure is a line.
fn lay_out_with(p: &Print, fonts: &Fonts, sizes: &BTreeMap<Hash, (u32, u32)>) -> Laid {
    layout::lay_out(p, fonts, &PageSpec::A4, &|h: &Hash| sizes.get(h).copied())
}

/// The warnings of the PDF of `p`, in their order. `images` are the figure images that print.
fn warnings<V>(p: &Print, images: &BTreeMap<Hash, V>, laid: &Laid) -> Vec<String> {
    fn figures<V>(p: &Print, images: &BTreeMap<Hash, V>, blocks: &[PBlock], out: &mut Vec<String>) {
        for b in blocks {
            match b {
                PBlock::Figure {
                    blob: Some(h), alt, ..
                } => {
                    if let Some(blob) = p.blobs.get(h)
                        && !images.contains_key(h)
                    {
                        out.push(match image::over_budget(&blob.media_type, &blob.bytes) {
                            Some((w, h)) => format!(
                                "the figure {alt:?} is {w}×{h} pixels, larger than a PDF export \
                                 prints ({}×{}, {} MiB decoded); the HTML and Markdown exports \
                                 show it",
                                image::MAX_SIDE,
                                image::MAX_SIDE,
                                image::MAX_DECODED >> 20
                            ),
                            None => format!(
                                "the figure {alt:?} is a {} image, which does not print in a \
                                 PDF; the HTML and Markdown exports show it",
                                blob.media_type
                            ),
                        });
                    }
                }
                PBlock::List { items, .. } => {
                    for i in items {
                        figures(p, images, i, out);
                    }
                }
                PBlock::Callout { blocks, .. } => figures(p, images, blocks, out),
                _ => {}
            }
        }
    }
    let mut out = p.warnings.clone();
    for d in &p.docs {
        figures(p, images, &d.blocks, &mut out);
    }
    if !laid.missing.is_empty() {
        let first: Vec<String> = laid.missing.iter().take(10).map(char::to_string).collect();
        out.push(format!(
            "{} characters have no glyph in the PDF fonts and print as ?: {}",
            laid.missing.len(),
            first.join(" ")
        ));
    }
    out
}

fn flate(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, LEVEL)
}

/// A DejaVu face in the file: the glyphs it draws, each with the first character drawn with it.
#[derive(Default)]
struct Subset {
    glyphs: BTreeMap<u16, char>,
}

/// The DejaVu subset a face draws with: 0 for Sans and Italic, 1 for Bold and Bold Italic; None
/// for Courier.
fn slot(face: Face) -> Option<usize> {
    match face {
        Face::Sans | Face::Italic => Some(0),
        Face::Bold | Face::BoldItalic => Some(1),
        Face::Mono => None,
    }
}

fn font_name(face: Face) -> &'static [u8] {
    match slot(face) {
        Some(0) => SANS_NAME,
        Some(_) => BOLD_NAME,
        None => MONO_NAME,
    }
}

/// The glyph `face` draws `c` with; `?`'s when it has none (the layout has already put `?` there).
fn glyph(fonts: &Fonts, face: Face, c: char) -> u16 {
    fonts
        .glyph(face, c)
        .or_else(|| fonts.glyph(face, '?'))
        .unwrap_or(0)
}

/// A six-letter subset tag that depends only on the face and its glyphs.
fn subset_tag(face: &str, glyphs: &[u16]) -> String {
    // FNV-1a, 64-bit
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in face
        .bytes()
        .chain(glyphs.iter().flat_map(|g| g.to_be_bytes()))
    {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (0..6)
        .map(|_| {
            let c = char::from(b'A' + (h % 26) as u8);
            h /= 26;
            c
        })
        .collect()
}

/// Writes one DejaVu subset: its Type0 font, CID font, descriptor, font file and ToUnicode map.
/// Returns the old glyph id → subset glyph id map.
fn write_font(
    pdf: &mut Pdf,
    next: &mut Ref,
    font_ref: Ref,
    fonts: &Fonts,
    i: usize,
    subset: &Subset,
) -> BTreeMap<u16, u16> {
    let (data, base, face) = if i == 0 {
        (SANS, "DejaVuSans", Face::Sans)
    } else {
        (SANS_BOLD, "DejaVuSans-Bold", Face::Bold)
    };
    let old: Vec<u16> = subset.glyphs.keys().copied().collect();
    let remapper = subsetter::GlyphRemapper::new_from_glyphs_sorted(&old);
    let file = subsetter::subset(data, 0, &remapper).expect("the compiled-in DejaVu font subsets");
    let map: BTreeMap<u16, u16> = old
        .iter()
        .map(|g| (*g, remapper.get(*g).unwrap_or(0)))
        .collect();
    let name = format!("{}+{base}", subset_tag(base, &old));
    let (cid_ref, desc_ref, file_ref, cmap_ref) =
        (next.bump(), next.bump(), next.bump(), next.bump());
    let info = SystemInfo {
        registry: Str(b"Adobe"),
        ordering: Str(b"Identity"),
        supplement: 0,
    };

    pdf.type0_font(font_ref)
        .base_font(Name(name.as_bytes()))
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(cid_ref)
        .to_unicode(cmap_ref);

    let mut cid = pdf.cid_font(cid_ref);
    cid.subtype(CidFontType::Type2)
        .base_font(Name(name.as_bytes()))
        .system_info(info)
        .font_descriptor(desc_ref)
        .default_width(0.0)
        .cid_to_gid_map_predefined(Name(b"Identity"));
    {
        // each run of consecutive subset ids, with its widths in 1/1000 of the size
        let mut runs: Vec<(u16, Vec<f32>)> = vec![];
        for (g, c) in &subset.glyphs {
            let (id, width) = (map[g], fonts.advance(face, *c, 1000.0));
            match runs.last_mut() {
                Some((start, ws)) if usize::from(*start) + ws.len() == usize::from(id) => {
                    ws.push(width);
                }
                _ => runs.push((id, vec![width])),
            }
        }
        let mut w = cid.widths();
        for (start, ws) in runs {
            w.consecutive(start, ws);
        }
    }
    cid.finish();

    let ttf = ttf_parser::Face::parse(data, 0).expect("the compiled-in DejaVu font parses");
    let unit = 1000.0 / f32::from(ttf.units_per_em());
    let bb = ttf.global_bounding_box();
    let cap = ttf.capital_height().unwrap_or(ttf.ascender());
    pdf.font_descriptor(desc_ref)
        .name(Name(name.as_bytes()))
        .flags(FontFlags::NON_SYMBOLIC)
        .bbox(Rect::new(
            f32::from(bb.x_min) * unit,
            f32::from(bb.y_min) * unit,
            f32::from(bb.x_max) * unit,
            f32::from(bb.y_max) * unit,
        ))
        .italic_angle(0.0)
        .ascent(fonts.ascent(face, 1000.0))
        .descent(fonts.descent(face, 1000.0))
        .cap_height(f32::from(cap) * unit)
        .stem_v(if i == 0 { 80.0 } else { 140.0 })
        .font_file2(file_ref);

    let packed = flate(&file);
    let length1 = i32::try_from(file.len()).unwrap_or(i32::MAX);
    pdf.stream(file_ref, &packed)
        .filter(Filter::FlateDecode)
        .pair(Name(b"Length1"), length1);

    let mut cmap = UnicodeCmap::new(Name(b"Custom"), info);
    for (g, c) in &subset.glyphs {
        cmap.pair(map[g], *c);
    }
    let cmap = flate(&cmap.finish().into_vec());
    pdf.stream(cmap_ref, &cmap).filter(Filter::FlateDecode);
    map
}

/// The XObject resource name of the `i`th image.
fn image_name(i: usize) -> String {
    format!("Im{i}")
}

fn render(p: &Print, fonts: &Fonts) -> Rendered {
    let mut pdf = Pdf::new();
    let mut next = Ref::new(1);
    let catalog_ref = next.bump();
    let tree_ref = next.bump();
    let info_ref = next.bump();

    // the images, one at a time (the task-9 ruling): each is decoded, written and its pixels
    // dropped before the next, so the export never holds more than one decoded image
    let mut sizes: BTreeMap<Hash, (u32, u32)> = BTreeMap::new();
    let mut image_index: BTreeMap<Hash, usize> = BTreeMap::new();
    let mut image_refs: Vec<Ref> = vec![];
    for (h, b) in &p.blobs {
        let Some(px) = image::decode(&b.media_type, &b.bytes) else {
            continue;
        };
        let r = next.bump();
        sizes.insert(*h, px.size());
        image_index.insert(*h, image_refs.len());
        image_refs.push(r);
        write_image(&mut pdf, &mut next, r, px);
    }
    let laid = lay_out_with(p, fonts, &sizes);
    let warnings = warnings(p, &sizes, &laid);

    // the glyphs each DejaVu subset draws, and whether Courier draws
    let mut subsets = [Subset::default(), Subset::default()];
    let mut mono = false;
    for item in laid.pages.iter().flat_map(|pg| &pg.items) {
        if let LItem::Text { face, text, .. } = item {
            match slot(*face) {
                Some(s) => {
                    for c in text.chars() {
                        subsets[s].glyphs.entry(glyph(fonts, *face, c)).or_insert(c);
                    }
                }
                None => mono = true,
            }
        }
    }

    // the fonts
    let mut font_refs: BTreeMap<&'static [u8], Ref> = BTreeMap::new();
    let mut maps: [BTreeMap<u16, u16>; 2] = [BTreeMap::new(), BTreeMap::new()];
    for (i, subset) in subsets.iter().enumerate() {
        if subset.glyphs.is_empty() {
            continue;
        }
        let r = next.bump();
        font_refs.insert(if i == 0 { SANS_NAME } else { BOLD_NAME }, r);
        maps[i] = write_font(&mut pdf, &mut next, r, fonts, i, subset);
    }
    if mono {
        let r = next.bump();
        font_refs.insert(MONO_NAME, r);
        pdf.type1_font(r)
            .base_font(Name(b"Courier"))
            .encoding_predefined(Name(b"WinAnsiEncoding"));
    }

    // the pages
    let page_refs: Vec<Ref> = laid.pages.iter().map(|_| next.bump()).collect();
    let spec = PageSpec::A4;
    let slant = SLANT.to_radians().tan();
    for (i, page) in laid.pages.iter().enumerate() {
        let content_ref = next.bump();
        let mut c = Content::new();
        let mut used_fonts: BTreeSet<&'static [u8]> = BTreeSet::new();
        let mut used_images: BTreeSet<usize> = BTreeSet::new();
        let mut links: Vec<(Ref, &[f32; 4], &Href)> = vec![];
        for item in &page.items {
            match item {
                LItem::Text {
                    x,
                    y,
                    face,
                    size,
                    text,
                } => {
                    let name = font_name(*face);
                    used_fonts.insert(name);
                    let bytes: Vec<u8> = match slot(*face) {
                        Some(s) => text
                            .chars()
                            .flat_map(|ch| {
                                let g = glyph(fonts, *face, ch);
                                maps[s].get(&g).copied().unwrap_or(0).to_be_bytes()
                            })
                            .collect(),
                        None => text
                            .chars()
                            .map(|ch| {
                                fonts
                                    .glyph(Face::Mono, ch)
                                    .and_then(|g| u8::try_from(g).ok())
                                    .unwrap_or(b'?')
                            })
                            .collect(),
                    };
                    let k = if matches!(face, Face::Italic | Face::BoldItalic) {
                        slant
                    } else {
                        0.0
                    };
                    c.begin_text()
                        .set_font(Name(name), *size)
                        .set_text_matrix([1.0, 0.0, k, 1.0, *x, *y])
                        .show(Str(&bytes))
                        .end_text();
                }
                LItem::Rule {
                    x1,
                    y1,
                    x2,
                    y2,
                    width,
                } => {
                    c.set_line_width(*width)
                        .move_to(*x1, *y1)
                        .line_to(*x2, *y2)
                        .stroke();
                }
                LItem::Image { x, y, w, h, blob } => {
                    let Some(n) = image_index.get(blob) else {
                        continue;
                    };
                    used_images.insert(*n);
                    c.save_state()
                        .transform([*w, 0.0, 0.0, *h, *x, *y])
                        .x_object(Name(image_name(*n).as_bytes()))
                        .restore_state();
                }
                LItem::Link { rect, href } => links.push((next.bump(), rect, href)),
            }
        }
        let content = flate(&c.finish().into_vec());
        pdf.stream(content_ref, &content)
            .filter(Filter::FlateDecode);

        let mut annots = vec![];
        for (r, rect, href) in links {
            // a link to a print page goes to its anchor, else to the page's start; a page the
            // layout does not hold gets no link
            let goto = match href {
                Href::Page { page, anchor } => {
                    let at = anchor
                        .as_ref()
                        .and_then(|a| laid.anchors.get(&(page.clone(), Some(a.clone()))))
                        .or_else(|| laid.anchors.get(&(page.clone(), None)));
                    let Some(at) = at else { continue };
                    Some(*at)
                }
                Href::External(_) => None,
            };
            let mut a = pdf.annotation(r);
            a.subtype(AnnotationType::Link)
                .rect(Rect::new(rect[0], rect[1], rect[2], rect[3]))
                .border(0.0, 0.0, 0.0, None);
            let mut action = a.action();
            match (goto, href) {
                (Some((pg, y)), _) => {
                    action
                        .action_type(ActionType::GoTo)
                        .destination()
                        .page(page_refs[pg])
                        .xyz(0.0, y, None);
                }
                (None, Href::External(u)) => {
                    action.action_type(ActionType::Uri).uri(Str(u.as_bytes()));
                }
                (None, Href::Page { .. }) => {}
            }
            action.finish();
            a.finish();
            annots.push(r);
        }

        let mut pg = pdf.page(page_refs[i]);
        pg.parent(tree_ref)
            .media_box(Rect::new(0.0, 0.0, spec.width, spec.height))
            .contents(content_ref);
        if !annots.is_empty() {
            pg.annotations(annots);
        }
        let mut res = pg.resources();
        if !used_fonts.is_empty() {
            let mut f = res.fonts();
            for name in &used_fonts {
                if let Some(r) = font_refs.get(name) {
                    f.pair(Name(name), *r);
                }
            }
        }
        if !used_images.is_empty() {
            let mut x = res.x_objects();
            for n in &used_images {
                x.pair(Name(image_name(*n).as_bytes()), image_refs[*n]);
            }
        }
    }
    pdf.pages(tree_ref)
        .kids(page_refs.iter().copied())
        .count(i32::try_from(page_refs.len()).unwrap_or(i32::MAX));

    // the outline
    let outline_ref = write_outline(&mut pdf, &mut next, &laid.outline, &page_refs);
    let mut catalog = pdf.catalog(catalog_ref);
    catalog.pages(tree_ref);
    if let Some(o) = outline_ref {
        catalog.outlines(o);
    }
    catalog.finish();
    pdf.document_info(info_ref)
        .producer(TextStr(&format!("rdy {}", env!("CARGO_PKG_VERSION"))))
        .title(TextStr(&p.title));

    Rendered {
        bytes: pdf.finish(),
        pages: laid.pages.len(),
        warnings,
        images: image_refs.len(),
    }
}

/// Writes one figure image as an image XObject at `r`, with its soft mask when it has alpha,
/// and drops its pixels.
fn write_image(pdf: &mut Pdf, next: &mut Ref, r: Ref, px: Pixels) {
    match px {
        Pixels::Jpeg {
            w,
            h,
            components,
            bytes,
        } => {
            pdf.image_xobject(r, &bytes)
                .width(dim(w))
                .height(dim(h))
                .color_space_name(Name(if components == 1 {
                    b"DeviceGray"
                } else {
                    b"DeviceRGB"
                }))
                .bits_per_component(8)
                .filter(Filter::DctDecode);
        }
        Pixels::Raw {
            w,
            h,
            gray,
            color,
            alpha,
        } => {
            let mask = alpha.map(|a| (next.bump(), flate(&a)));
            let packed = flate(&color);
            drop(color);
            let mut x = pdf.image_xobject(r, &packed);
            x.width(dim(w))
                .height(dim(h))
                .color_space_name(Name(if gray { b"DeviceGray" } else { b"DeviceRGB" }))
                .bits_per_component(8)
                .filter(Filter::FlateDecode);
            if let Some((m, _)) = &mask {
                x.s_mask(*m);
            }
            x.finish();
            if let Some((m, packed)) = &mask {
                pdf.image_xobject(*m, packed)
                    .width(dim(w))
                    .height(dim(h))
                    .color_space_name(Name(b"DeviceGray"))
                    .bits_per_component(8)
                    .filter(Filter::FlateDecode);
            }
        }
    }
}

/// An image dimension as the PDF writes it.
fn dim(n: u32) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// The outline: one item per entry, each the child of the nearest entry before it with a lower
/// level. Each item is closed; it opens on its children. None when there is no entry.
fn write_outline(
    pdf: &mut Pdf,
    next: &mut Ref,
    entries: &[OutlineEntry],
    page_refs: &[Ref],
) -> Option<Ref> {
    if entries.is_empty() {
        return None;
    }
    let root = next.bump();
    let refs: Vec<Ref> = entries.iter().map(|_| next.bump()).collect();
    // each entry's parent (None: the root)
    let mut parent: Vec<Option<usize>> = vec![];
    let mut stack: Vec<usize> = vec![];
    for (i, e) in entries.iter().enumerate() {
        while let Some(&top) = stack.last() {
            if entries[top].level < e.level {
                break;
            }
            stack.pop();
        }
        parent.push(stack.last().copied());
        stack.push(i);
    }
    // each entry's children in order, and the root's (the last list)
    // and each entry's place among its siblings
    let mut kids_of: Vec<Vec<usize>> = vec![vec![]; entries.len() + 1];
    let mut place = vec![0; entries.len()];
    for (i, p) in parent.iter().enumerate() {
        let kids = &mut kids_of[p.unwrap_or(entries.len())];
        place[i] = kids.len();
        kids.push(i);
    }
    let children = |of: Option<usize>| -> &[usize] { &kids_of[of.unwrap_or(entries.len())] };
    let top = children(None);
    let mut o = pdf.outline(root);
    o.first(refs[top[0]])
        .last(refs[top[top.len() - 1]])
        .count(i32::try_from(top.len()).unwrap_or(i32::MAX));
    o.finish();
    for (i, e) in entries.iter().enumerate() {
        let (siblings, at) = (children(parent[i]), place[i]);
        let kids = children(Some(i));
        let mut item = pdf.outline_item(refs[i]);
        item.title(TextStr(&e.title))
            .parent(parent[i].map_or(root, |p| refs[p]));
        if at > 0 {
            item.prev(refs[siblings[at - 1]]);
        }
        if let Some(n) = siblings.get(at + 1) {
            item.next(refs[*n]);
        }
        if let (Some(f), Some(l)) = (kids.first(), kids.last()) {
            item.first(refs[*f])
                .last(refs[*l])
                .count(-i32::try_from(kids.len()).unwrap_or(i32::MAX));
        }
        item.dest().page(page_refs[e.page]).xyz(0.0, e.y, None);
    }
    Some(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::layout::LItem;
    use crate::print::{PBlock, PageKind, PrintDoc, Run};
    use std::collections::BTreeMap;

    /// One indirect object of a PDF: its dictionary's text and its stream's bytes.
    struct Obj {
        dict: String,
        stream: Option<Vec<u8>>,
    }

    fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
        hay[from..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|i| from + i)
    }

    /// Every object, read where the cross-reference table says it starts.
    fn objects(pdf: &[u8]) -> BTreeMap<u32, Obj> {
        let sx = find(pdf, b"startxref\n", pdf.len() - 64).expect("startxref") + 10;
        let end = find(pdf, b"\n", sx).unwrap();
        let xref: usize = std::str::from_utf8(&pdf[sx..end]).unwrap().parse().unwrap();
        let table = std::str::from_utf8(&pdf[xref..sx]).unwrap();
        let mut tokens = table.split_whitespace();
        assert_eq!(tokens.next(), Some("xref"));
        assert_eq!(tokens.next(), Some("0"));
        let n: u32 = tokens.next().unwrap().parse().unwrap();
        let mut out = BTreeMap::new();
        let length = regex::Regex::new(r"/Length (\d+)").unwrap();
        for id in 0..n {
            let offset: usize = tokens.next().unwrap().parse().unwrap();
            let _gen = tokens.next().unwrap();
            if tokens.next().unwrap() != "n" {
                continue;
            }
            let head = format!("{id} 0 obj\n");
            assert_eq!(&pdf[offset..offset + head.len()], head.as_bytes());
            let start = offset + head.len();
            let endobj = find(pdf, b"\nendobj", start).unwrap();
            let obj = match find(pdf, b">>\nstream\n", start) {
                Some(s) if s < endobj => {
                    let dict = String::from_utf8(pdf[start..s + 2].to_vec()).unwrap();
                    let len: usize = length.captures(&dict).expect("a stream's length")[1]
                        .parse()
                        .unwrap();
                    let data = s + 10;
                    Obj {
                        dict,
                        stream: Some(pdf[data..data + len].to_vec()),
                    }
                }
                _ => Obj {
                    dict: String::from_utf8(pdf[start..endobj].to_vec()).unwrap(),
                    stream: None,
                },
            };
            out.insert(id, obj);
        }
        out
    }

    fn inflate(o: &Obj) -> Vec<u8> {
        let data = o.stream.as_ref().expect("a stream");
        assert!(o.dict.contains("/Filter /FlateDecode"), "{}", o.dict);
        miniz_oxide::inflate::decompress_to_vec_zlib(data).expect("the stream inflates")
    }

    fn refs(dict: &str, pattern: &str) -> Vec<(String, u32)> {
        regex::Regex::new(pattern)
            .unwrap()
            .captures_iter(dict)
            .map(|c| (c[1].to_string(), c[2].parse().unwrap()))
            .collect()
    }

    fn one_ref(dict: &str, key: &str) -> u32 {
        refs(dict, &format!(r"/({key}) (\d+) 0 R"))
            .first()
            .unwrap_or_else(|| panic!("{key} in {dict}"))
            .1
    }

    /// The page objects in order: the catalog's page tree's kids.
    fn page_ids(objs: &BTreeMap<u32, Obj>) -> Vec<u32> {
        let catalog = objs
            .values()
            .find(|o| o.dict.contains("/Type /Catalog"))
            .expect("a catalog");
        let pages = &objs[&one_ref(&catalog.dict, "Pages")].dict;
        let kids = regex::Regex::new(r"/Kids \[([^\]]*)\]")
            .unwrap()
            .captures(pages)
            .unwrap()[1]
            .to_string();
        regex::Regex::new(r"(\d+) 0 R")
            .unwrap()
            .captures_iter(&kids)
            .map(|c| c[1].parse().unwrap())
            .collect()
    }

    /// A content stream's tokens: names, numbers, strings (as bytes) and operators.
    #[derive(Debug, Clone, PartialEq)]
    enum Tok {
        Name(String),
        Num(f32),
        Str(Vec<u8>),
        Op(String),
    }

    fn tokens(c: &[u8]) -> Vec<Tok> {
        let mut out = vec![];
        let mut i = 0;
        while i < c.len() {
            let b = c[i];
            if b.is_ascii_whitespace() {
                i += 1;
            } else if b == b'/' {
                let s = i + 1;
                i = s;
                while i < c.len() && !c[i].is_ascii_whitespace() && !b"/[]<>()".contains(&c[i]) {
                    i += 1;
                }
                out.push(Tok::Name(String::from_utf8(c[s..i].to_vec()).unwrap()));
            } else if b == b'<' {
                let e = find(c, b">", i).unwrap();
                let hex: Vec<u8> = c[i + 1..e]
                    .iter()
                    .copied()
                    .filter(|b| !b.is_ascii_whitespace())
                    .collect();
                let bytes = hex
                    .chunks(2)
                    .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
                    .collect();
                out.push(Tok::Str(bytes));
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
            } else if b == b'-' || b == b'.' || b.is_ascii_digit() {
                let s = i;
                i += 1;
                while i < c.len() && (c[i] == b'.' || c[i].is_ascii_digit()) {
                    i += 1;
                }
                out.push(Tok::Num(
                    std::str::from_utf8(&c[s..i]).unwrap().parse().unwrap(),
                ));
            } else {
                let s = i;
                while i < c.len() && !c[i].is_ascii_whitespace() && !b"/[]<>()".contains(&c[i]) {
                    i += 1;
                }
                if i == s {
                    i += 1;
                    continue;
                }
                out.push(Tok::Op(String::from_utf8(c[s..i].to_vec()).unwrap()));
            }
        }
        out
    }

    /// A ToUnicode map's code → text, from its `bfchar` entries.
    fn to_unicode(cmap: &[u8]) -> BTreeMap<u16, String> {
        let text = String::from_utf8(cmap.to_vec()).unwrap();
        let mut out = BTreeMap::new();
        let pair = regex::Regex::new(r"<([0-9A-Fa-f]{4})>\s*<([0-9A-Fa-f]+)>").unwrap();
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

    fn run(text: &str, bold: bool, italic: bool, code: bool) -> Run {
        Run {
            text: text.into(),
            bold,
            italic,
            code,
            link: None,
        }
    }

    fn sample() -> Print {
        let blocks = vec![
            PBlock::Heading {
                level: 2,
                anchor: Some("start".into()),
                runs: vec![run("Arrows → and ≤ signs", false, false, false)],
            },
            PBlock::Para {
                anchor: None,
                runs: vec![
                    run("Plain words, é and α; ", false, false, false),
                    run("bold words ", true, false, false),
                    run("slanted words ", false, true, false),
                    run("both at once ", true, true, false),
                    run("code_here(1) ", false, false, true),
                    run(
                        "and 中 has no glyph, nor does ☃ in code: ",
                        false,
                        false,
                        false,
                    ),
                    run("x → ☃ (é)", false, false, true),
                ],
            },
            PBlock::Code {
                anchor: None,
                lang: Some("rust".into()),
                text: "fn main() {\n\tlet s = \"(a) \\\\ b\";\n}\n".into(),
            },
            PBlock::List {
                ordered: true,
                start: 9,
                items: vec![
                    vec![PBlock::Para {
                        anchor: None,
                        runs: vec![run("ninth", false, false, false)],
                    }],
                    vec![PBlock::Para {
                        anchor: None,
                        runs: vec![run("tenth — last", false, false, false)],
                    }],
                ],
            },
        ];
        Print {
            title: "Readback “quoted”".into(),
            description: Some("A print that reads back".into()),
            label: "v1.0.0".into(),
            scope: "canon".into(),
            tag_redacted: false,
            docs: vec![
                PrintDoc {
                    kind: PageKind::Doc,
                    id: "d-1".into(),
                    title: "First doc".into(),
                    page: "first.html".into(),
                    depth: 0,
                    blocks: blocks.clone(),
                },
                PrintDoc {
                    kind: PageKind::Doc,
                    id: "d-2".into(),
                    title: "Second doc".into(),
                    page: "second.html".into(),
                    depth: 0,
                    blocks,
                },
            ],
            blobs: BTreeMap::new(),
            warnings: vec![],
            rdy_dir: std::path::PathBuf::from("/nowhere/.rdy"),
        }
    }

    /// One run of text a page draws: its text, its text matrix and whether its font is bold.
    type Drawn = (String, Vec<f32>, bool);

    /// The text each page of `pdf` draws, in page order: each `Tj` string decoded through its
    /// font's ToUnicode map (an embedded font) or Courier's WinAnsiEncoding (the code face).
    fn page_text(pdf: &[u8], fonts: &Fonts) -> Vec<Vec<Drawn>> {
        let objs = objects(pdf);
        // Courier's WinAnsiEncoding: byte → character
        let mut win_ansi = BTreeMap::new();
        for c in (0u32..0x3000).filter_map(char::from_u32) {
            if let Some(g) = fonts.glyph(Face::Mono, c) {
                win_ansi.insert(u8::try_from(g).unwrap(), c);
            }
        }
        let mut out = vec![];
        for id in page_ids(&objs) {
            let page = &objs[&id].dict;
            let fonts_of: BTreeMap<String, u32> =
                refs(page, r"/(F\d+) (\d+) 0 R").into_iter().collect();
            let content = inflate(&objs[&one_ref(page, "Contents")]);
            let toks = tokens(&content);
            let mut font = String::new();
            let mut matrix = vec![];
            let mut read = vec![];
            for (k, t) in toks.iter().enumerate() {
                match t {
                    Tok::Op(op) if op == "Tf" => {
                        let Tok::Name(n) = &toks[k - 2] else {
                            panic!("Tf takes a name")
                        };
                        font = n.clone();
                    }
                    Tok::Op(op) if op == "Tm" => {
                        matrix = toks[k - 6..k]
                            .iter()
                            .map(|t| match t {
                                Tok::Num(n) => *n,
                                other => panic!("{other:?}"),
                            })
                            .collect();
                    }
                    Tok::Op(op) if op == "Tj" => {
                        let Tok::Str(bytes) = &toks[k - 1] else {
                            panic!("Tj takes a string")
                        };
                        let f = &objs[&fonts_of[&font]].dict;
                        let text: String = if f.contains("/Subtype /Type0") {
                            assert!(f.contains("/Encoding /Identity-H"), "{f}");
                            let map = to_unicode(&inflate(&objs[&one_ref(f, "ToUnicode")]));
                            bytes
                                .chunks(2)
                                .map(|g| {
                                    let g = u16::from_be_bytes([g[0], g[1]]);
                                    map.get(&g)
                                        .unwrap_or_else(|| panic!("glyph {g} is in ToUnicode"))
                                        .clone()
                                })
                                .collect()
                        } else {
                            assert!(f.contains("/BaseFont /Courier"), "{f}");
                            assert!(f.contains("/Encoding /WinAnsiEncoding"), "{f}");
                            bytes.iter().map(|b| win_ansi[b]).collect()
                        };
                        read.push((text, matrix.clone(), f.contains("Bold")));
                    }
                    _ => {}
                }
            }
            out.push(read);
        }
        out
    }

    #[test]
    fn ec9_page_text_reads_back_through_to_unicode() {
        let fonts = Fonts::load();
        let p = sample();
        let laid = lay_out(&p, &fonts);
        let (pdf, pages, warnings) = pdf_bytes(&p, &fonts);
        assert_eq!(pages, laid.pages.len());
        assert!(pages >= 3, "the title part and two docs");
        assert_eq!(
            warnings,
            vec!["3 characters have no glyph in the PDF fonts and print as ?: → ☃ 中".to_string()]
        );
        let objs = objects(&pdf);
        let page_ids = page_ids(&objs);
        assert_eq!(page_ids.len(), pages);
        let slant = 12f32.to_radians().tan();
        let mut faces_seen = std::collections::BTreeSet::new();
        for (i, read) in page_text(&pdf, &fonts).iter().enumerate() {
            let want: Vec<(String, Face)> = laid.pages[i]
                .items
                .iter()
                .filter_map(|it| match it {
                    LItem::Text { text, face, .. } => Some((text.clone(), *face)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                read.iter().map(|r| r.0.clone()).collect::<Vec<_>>(),
                want.iter().map(|w| w.0.clone()).collect::<Vec<_>>(),
                "page {i}"
            );
            // the face of each run: a slanted face has the 12° text matrix, a bold one the Bold
            // font
            for ((_, m, bold), (_, face)) in read.iter().zip(&want) {
                faces_seen.insert(*face);
                let slanted = matches!(face, Face::Italic | Face::BoldItalic);
                let c = if slanted { slant } else { 0.0 };
                assert!((m[2] - c).abs() < 1e-4, "{face:?}: {m:?}");
                if *face != Face::Mono {
                    assert_eq!(*bold, matches!(face, Face::Bold | Face::BoldItalic));
                }
            }
        }
        assert_eq!(faces_seen.len(), 5, "every face: {faces_seen:?}");
    }

    /// A print of one doc whose blocks are these figures, each `(media type, bytes, alt)`.
    fn figures(figs: Vec<(&str, Vec<u8>, &str)>) -> Print {
        let mut p = sample();
        p.docs.truncate(1);
        p.docs[0].blocks = vec![];
        for (media_type, bytes, alt) in figs {
            let h = Hash::of(&bytes);
            p.docs[0].blocks.push(PBlock::Figure {
                anchor: None,
                blob: Some(h),
                alt: alt.into(),
                caption: vec![],
            });
            p.blobs.insert(
                h,
                crate::print::Blob {
                    media_type: media_type.into(),
                    bytes,
                    file: format!("assets/{h}"),
                },
            );
        }
        p
    }

    /// The image objects of a PDF, each with its soft mask's object when it has one.
    fn image_objects(objs: &BTreeMap<u32, Obj>) -> Vec<(&Obj, Option<&Obj>)> {
        let masks: BTreeSet<u32> = objs
            .values()
            .filter_map(|o| refs(&o.dict, r"/(SMask) (\d+) 0 R").first().map(|r| r.1))
            .collect();
        objs.iter()
            .filter(|(id, o)| o.dict.contains("/Subtype /Image") && !masks.contains(id))
            .map(|(_, o)| {
                let mask = refs(&o.dict, r"/(SMask) (\d+) 0 R")
                    .first()
                    .map(|r| &objs[&r.1]);
                (o, mask)
            })
            .collect()
    }

    fn figure_lines(laid: &Laid) -> Vec<String> {
        laid.pages
            .iter()
            .flat_map(|pg| &pg.items)
            .filter_map(|it| match it {
                LItem::Text { text, .. } if text.starts_with("Figure: ") => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn ec9_arabic_prints_as_question_marks_in_the_glyph_warning() {
        // final review I1: RULING F1 prints the scripts that need shaping as `?` with the glyph
        // warning, although DejaVu holds Arabic letters
        let line = "Arabic: العربية. Devanagari: हिन्दी. Greek: αβγ. Cyrillic: Привет. CJK: 中文.";
        let mut p = sample();
        p.title = "العربية".into();
        p.docs[0].title = "باب العربية".into();
        p.docs[0].blocks = vec![
            PBlock::Para {
                anchor: None,
                runs: vec![run(line, false, false, false)],
            },
            PBlock::Para {
                anchor: None,
                runs: vec![
                    run("bold العربية ", true, false, false),
                    run("slanted العربية", false, true, false),
                ],
            },
            PBlock::Figure {
                anchor: None,
                blob: None,
                alt: "صورة".into(),
                caption: vec![],
            },
        ];
        p.docs.truncate(1);
        let fonts = Fonts::load();
        let laid = lay_out(&p, &fonts);
        let texts: Vec<&str> = laid
            .pages
            .iter()
            .flat_map(|pg| &pg.items)
            .filter_map(|it| match it {
                LItem::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            texts.contains(
                &"Arabic: ???????. Devanagari: ??????. Greek: αβγ. Cyrillic: Привет. CJK: ??."
            ),
            "{texts:?}"
        );
        assert!(texts.contains(&"Figure: ????"), "{texts:?}");
        let (pdf, _, warnings) = pdf_bytes(&p, &fonts);
        // 17: nine Arabic letters, six Devanagari signs and two CJK; the ten lowest code points
        // (O33) are the nine Arabic letters and Devanagari's द
        assert_eq!(
            warnings,
            vec![
                "17 characters have no glyph in the PDF fonts and print as ?: ا ب ة ر ص ع ل و ي द"
                    .to_string()
            ]
        );
        // no font of the file maps a glyph to an Arabic letter
        let objs = objects(&pdf);
        let mut maps = 0;
        for o in objs.values().filter(|o| o.dict.contains("/ToUnicode")) {
            let map = to_unicode(&inflate(&objs[&one_ref(&o.dict, "ToUnicode")]));
            maps += 1;
            for text in map.values() {
                assert!(
                    !text
                        .chars()
                        .any(|c| matches!(u32::from(c), 0x0590..=0x08FF)),
                    "{text:?} is in a ToUnicode map"
                );
            }
        }
        assert!(maps > 0, "the file embeds a DejaVu font");
    }

    #[test]
    fn ec9_a_trns_colour_key_prints_as_a_soft_mask() {
        use crate::print::image::tests::{filter, png_with};
        let rgb = vec![
            vec![0u8, 0, 0, 9, 9, 9, 0, 0, 0],
            vec![1, 0, 0, 0, 0, 0, 0, 0, 0],
        ];
        let gray = vec![vec![0u8, 7, 0, 7]];
        let p = figures(vec![
            (
                "image/png",
                png_with(3, 2, 8, 2, 0, &filter(&rgb, 3), &[(b"tRNS", vec![0; 6])]),
                "black is see-through",
            ),
            (
                "image/png",
                png_with(4, 1, 8, 0, 0, &filter(&gray, 1), &[(b"tRNS", vec![0, 7])]),
                "seven is see-through",
            ),
        ]);
        let fonts = Fonts::load();
        let (pdf, _, warnings) = pdf_bytes(&p, &fonts);
        assert!(warnings.is_empty(), "{warnings:?}");
        let objs = objects(&pdf);
        let images = image_objects(&objs);
        assert_eq!(images.len(), 2);
        for (color, mask) in images {
            let mask = mask.unwrap_or_else(|| panic!("a soft mask: {}", color.dict));
            assert!(
                mask.dict.contains("/ColorSpace /DeviceGray"),
                "{}",
                mask.dict
            );
            if color.dict.contains("/Width 3") {
                assert!(color.dict.contains("/ColorSpace /DeviceRGB"));
                assert_eq!(inflate(color), rgb.concat());
                assert_eq!(inflate(mask), vec![0, 255, 0, 255, 0, 0]);
            } else {
                assert!(color.dict.contains("/ColorSpace /DeviceGray"));
                assert_eq!(inflate(color), gray.concat());
                assert_eq!(inflate(mask), vec![255, 0, 255, 0]);
            }
        }
    }

    #[test]
    fn ec9_a_whole_jpeg_prints_and_a_cut_one_leaves_a_line_and_a_warning() {
        use crate::print::image::tests::{
            BASELINE_GRAY, BASELINE_RGB, PROGRESSIVE_RGB, after_segment,
        };
        let cut = BASELINE_RGB[..after_segment(BASELINE_RGB, 0xc0)].to_vec();
        let p = figures(vec![
            ("image/jpeg", BASELINE_GRAY.to_vec(), "gray"),
            ("image/jpeg", cut, "cut short"),
            ("image/jpeg", PROGRESSIVE_RGB.to_vec(), "progressive"),
        ]);
        let fonts = Fonts::load();
        let (pdf, _, warnings) = pdf_bytes(&p, &fonts);
        assert_eq!(
            warnings,
            vec![
                "the figure \"cut short\" is a image/jpeg image, which does not print in a PDF; \
                 the HTML and Markdown exports show it"
                    .to_string()
            ]
        );
        assert_eq!(
            figure_lines(&lay_out(&p, &fonts)),
            vec!["Figure: cut short"]
        );
        let objs = objects(&pdf);
        let mut streams: Vec<Vec<u8>> = image_objects(&objs)
            .into_iter()
            .map(|(o, mask)| {
                assert!(mask.is_none());
                assert!(o.dict.contains("/Filter /DCTDecode"), "{}", o.dict);
                o.stream.clone().unwrap()
            })
            .collect();
        streams.sort();
        let mut want = vec![BASELINE_GRAY.to_vec(), PROGRESSIVE_RGB.to_vec()];
        want.sort();
        assert_eq!(streams, want);
    }

    #[test]
    fn ec9_an_image_over_the_budget_leaves_a_line_and_its_warning() {
        use crate::print::image::tests::png;
        let mut wide = vec![0u8];
        wide.extend(std::iter::repeat_n(3u8, 8193));
        let mut ok = vec![0u8];
        ok.extend(std::iter::repeat_n(3u8, 8192));
        let p = figures(vec![
            ("image/png", png(8193, 1, 8, 0, 0, &wide), "too wide"),
            ("image/png", png(8192, 1, 8, 0, 0, &ok), "wide enough"),
        ]);
        let fonts = Fonts::load();
        let (pdf, _, warnings) = pdf_bytes(&p, &fonts);
        assert_eq!(
            warnings,
            vec![
                "the figure \"too wide\" is 8193×1 pixels, larger than a PDF export prints \
                 (8192×8192, 64 MiB decoded); the HTML and Markdown exports show it"
                    .to_string()
            ]
        );
        assert_eq!(figure_lines(&lay_out(&p, &fonts)), vec!["Figure: too wide"]);
        let objs = objects(&pdf);
        let images = image_objects(&objs);
        assert_eq!(images.len(), 1);
        assert!(images[0].0.dict.contains("/Width 8192"));
    }

    /// pub8 (D-3M81W, RULING F3): the title page of a named profile's print at an affected
    /// chosen tag reads back `Version: <tag> (redacted)`, glyph by glyph; an unaffected tag's
    /// reads `Version: <tag>`, and no label leaves no version line.
    #[test]
    fn pub8_pdf_title_page_reads_back_the_redacted_marker() {
        let fonts = Fonts::load();
        let title_page = |label: &str, redacted: bool| -> Vec<String> {
            let mut p = sample();
            p.label = label.into();
            p.scope = String::new();
            p.tag_redacted = redacted;
            let (pdf, _, _) = pdf_bytes(&p, &fonts);
            page_text(&pdf, &fonts)[0]
                .iter()
                .map(|r| r.0.clone())
                .collect()
        };
        let head = ["Readback “quoted”", "A print that reads back"];
        let want = |version: Option<&str>| -> Vec<String> {
            let mut out: Vec<String> = head.iter().map(|s| s.to_string()).collect();
            out.extend(version.map(str::to_string));
            out.push("1".into());
            out
        };
        assert_eq!(
            title_page("v1.0.0", true),
            want(Some("Version: v1.0.0 (redacted)"))
        );
        assert_eq!(title_page("v1.0.0", false), want(Some("Version: v1.0.0")));
        assert_eq!(title_page("", false), want(None));
        assert_eq!(title_page("", true), want(None));
    }

    /// pub8 (D-5CVWB): the PDF of a named profile's print draws the selected text, read back
    /// glyph by glyph through the same decoding as `ec9_page_text_reads_back_through_to_unicode`,
    /// and no glyph of a block outside the selection, of another profile or of the project's name.
    #[test]
    fn pub8_pdf_text_reads_back_from_the_selected_print() {
        use rdy_engine::answer::{AnswerRequest, relay::RelayInput};
        use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
        use rdy_engine::{Rdy, Scope, Session, SessionKind, WriteRequest};
        use rdy_model::Role;
        let session = |actor: &str, role| Session {
            actor: actor.into(),
            run: None,
            role,
            kind: SessionKind::Human,
        };
        let (agent, founder) = (
            session("agent:a", Role::Writer),
            session("human:founder", Role::Approver),
        );
        let write = |r: &mut Rdy, view: &str, ops: serde_json::Value| {
            r.write(
                &agent,
                WriteRequest {
                    view: view.into(),
                    ops: serde_json::from_value(ops).unwrap(),
                    justification: None,
                    message: format!("write to {view}"),
                },
            )
            .unwrap_or_else(|e| panic!("write to {view}: {e}"))
            .created
        };
        // lands `view`, confirming each exception the founder's queue holds for it
        let land = |r: &mut Rdy, view: &str| {
            let p = r.propose(&agent, view, view, Some("test summary")).unwrap();
            let q = r
                .read()
                .pending(&PendingQuery {
                    person: founder.actor.clone(),
                    session_ref: None,
                    max_tokens: CARD_MAX_TOKENS,
                })
                .unwrap();
            let refs: Vec<String> = q
                .items
                .iter()
                .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(&*p.r#ref))
                .map(|i| i.r#ref.clone())
                .collect();
            for item in refs {
                let req = AnswerRequest {
                    r#ref: item,
                    option: "confirm".into(),
                    card_hash: None,
                    note: None,
                    relay: RelayInput::default(),
                    brief: None,
                };
                r.answer(&founder, &founder.actor, req).unwrap();
            }
            r.land(&founder, &p.id).unwrap();
        };
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let ids = write(
            &mut r,
            "w1",
            serde_json::json!([
                {"op": "create", "type": "doc", "id": "$sib", "fields": {"name": "sibling", "title": "SIBCANARY title", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$sp", "parent": "$sib", "body": "SIBCANARY prose."},
                {"op": "create", "type": "code", "parent": "$sib", "fields": {"lang": "python"}, "body": "SIBCANARY_CODE = 2"},
                {"op": "create", "type": "doc", "id": "$chosen", "fields": {"name": "chosen", "title": "Chosen page", "doc_type": "page"}},
                {"op": "create", "type": "section", "parent": "$chosen", "fields": {"title": "Chosen section"}},
                {"op": "create", "type": "prose", "parent": "$chosen", "body": "Chosen readback words, é and α."},
                {"op": "create", "type": "prose", "parent": "$chosen", "body": "Shown {{$sp|display words}} and bare {{$sp}}."},
                {"op": "create", "type": "code", "parent": "$chosen", "fields": {"lang": "python"}, "body": "chosen_code = 1"}
            ]),
        );
        land(&mut r, "w1");
        let admin = write(
            &mut r,
            "admin",
            serde_json::json!([{"op": "create", "type": "doc", "id": "$admin",
                "fields": {"name": "admin", "title": "ADMINCANARY page", "doc_type": "page"}}]),
        )["$admin"]
            .clone();
        write(
            &mut r,
            "admin",
            serde_json::json!([{"op": "create", "type": "publication_profile", "parent": admin,
                "fields": {"key": "readback", "title": "Readback title",
                    "description": "Readback description.", "audience": "org",
                    "select": [ids["$chosen"].clone()]}}]),
        );
        land(&mut r, "admin");
        let fonts = Fonts::load();
        let read = r.read();
        let drawn = |p: &Print| -> String {
            let (pdf, pages, _) = pdf_bytes(p, &fonts);
            let text = page_text(&pdf, &fonts);
            assert_eq!(text.len(), pages);
            text.iter()
                .flatten()
                .map(|d| d.0.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        };

        // the canaries are real: the full-source PDF draws them (RULING F2)
        let full = drawn(&crate::print::build(&read, &Scope::Canon).unwrap());
        for word in ["SIBCANARY", "SIBCANARY_CODE", "ADMINCANARY"] {
            assert!(full.contains(word), "{word}: {full}");
        }

        let p = crate::print::build_profile(&read, "canon", "readback").unwrap();
        let (pdf, _, _) = pdf_bytes(&p, &fonts);
        let text = drawn(&p);
        for want in [
            "Readback title",
            "Readback description.",
            "Chosen page",
            "Chosen section",
            "Chosen readback words, é and α.",
            "Shown display words and bare unavailable reference.",
            "chosen_code = 1",
        ] {
            assert!(text.contains(want), "{want}: {text}");
        }
        // no glyph and no raw byte of an excluded block, its id, the profile's home or the
        // project's own source scope
        let mut canaries = vec![
            "SIBCANARY".to_string(),
            "ADMINCANARY".into(),
            "Version:".into(),
            "Scope:".into(),
            "canon".into(),
        ];
        canaries.extend([&ids["$sib"], &ids["$sp"], &admin].map(|s| s.to_string()));
        for word in &canaries {
            assert!(!text.contains(word.as_str()), "{word}: {text}");
            assert!(
                find(&pdf, word.as_bytes(), 0).is_none(),
                "{word} in the bytes"
            );
        }
    }
}
