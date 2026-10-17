//! ec9 (D-6SANB, RULING F1): the print as one PDF file, written natively with no browser. The
//! file's cross-reference table is valid, each page of the layout is a page, the fonts are
//! embedded subsets with ToUnicode maps, links go to their anchor's page, and two exports are
//! the same bytes.
use rdy_engine::{E_ALREADY_EXISTS, E_BAD_INPUT, Rdy, Scope};
use rdy_qa::golden::*;
use rdy_render::print::fonts::Fonts;
use rdy_render::print::layout::{LItem, Laid};
use rdy_render::print::{self, Href, Print};
use regex::Regex;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn canon(r: &Rdy) -> Print {
    print::build(&r.read(), &Scope::Canon).expect("print canon")
}

fn pdf_of(p: &Print) -> (Vec<u8>, usize, Vec<String>) {
    print::pdf::pdf_bytes(p, &Fonts::load())
}

fn laid_of(p: &Print) -> Laid {
    print::pdf::lay_out(p, &Fonts::load())
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| from + i)
}

/// `startxref`'s offset.
fn startxref(pdf: &[u8]) -> usize {
    let at = find(pdf, b"startxref\n", pdf.len().saturating_sub(64)).expect("startxref") + 10;
    let end = find(pdf, b"\n", at).expect("a line end");
    std::str::from_utf8(&pdf[at..end])
        .unwrap()
        .parse()
        .expect("a number")
}

/// The cross-reference table: each object number in use and its offset.
fn xref(pdf: &[u8]) -> BTreeMap<u32, usize> {
    let at = startxref(pdf);
    let end = find(pdf, b"trailer\n", at).expect("a trailer");
    let head_end = find(pdf, b"\n", at + 5).expect("the section line") + 1;
    let head = std::str::from_utf8(&pdf[at..head_end]).unwrap();
    let head: Vec<u32> = head
        .strip_prefix("xref\n")
        .expect("xref")
        .trim_end()
        .split(' ')
        .map(|n| n.parse().unwrap())
        .collect();
    assert_eq!(head[0], 0, "one section from object 0");
    // each entry is 20 bytes: 10 digits, a space, 5 digits, a space, n or f, CR LF
    let entries = &pdf[head_end..end];
    assert_eq!(
        entries.len(),
        20 * head[1] as usize,
        "as many entries as it says"
    );
    let mut out = BTreeMap::new();
    for (id, e) in entries.chunks(20).enumerate() {
        let e = std::str::from_utf8(e).unwrap();
        assert!(e.ends_with("\r\n"), "{e:?}");
        let parts: Vec<&str> = e.trim_end().split(' ').collect();
        assert_eq!(parts.len(), 3, "{e:?}");
        assert_eq!((parts[0].len(), parts[1].len()), (10, 5), "{e:?}");
        if parts[2] == "n" {
            out.insert(id as u32, parts[0].parse().unwrap());
        } else {
            assert_eq!(parts[2], "f", "{e:?}");
        }
    }
    out
}

/// One object: its dictionary's text and its stream's bytes, read at its offset.
struct Obj {
    dict: String,
    stream: Option<Vec<u8>>,
}

fn objects(pdf: &[u8]) -> BTreeMap<u32, Obj> {
    let mut out = BTreeMap::new();
    let len_re = Regex::new(r"/Length (\d+)").unwrap();
    for (id, offset) in xref(pdf) {
        let head = format!("{id} 0 obj\n");
        assert_eq!(
            &pdf[offset..offset + head.len()],
            head.as_bytes(),
            "object {id}"
        );
        let start = offset + head.len();
        let endobj = find(pdf, b"\nendobj", start).unwrap();
        let obj = match find(pdf, b">>\nstream\n", start) {
            Some(s) if s < endobj => {
                let dict = String::from_utf8_lossy(&pdf[start..s + 2]).into_owned();
                let len: usize = len_re.captures(&dict).expect("a length")[1]
                    .parse()
                    .unwrap();
                let data = s + 10;
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

fn ref_of(dict: &str, key: &str) -> Option<u32> {
    Regex::new(&format!(r"/{key} (\d+) 0 R"))
        .unwrap()
        .captures(dict)
        .map(|c| c[1].parse().unwrap())
}

fn ref_list(text: &str) -> Vec<u32> {
    Regex::new(r"(\d+) 0 R")
        .unwrap()
        .captures_iter(text)
        .map(|c| c[1].parse().unwrap())
        .collect()
}

fn array_of<'a>(dict: &'a str, key: &str) -> Option<&'a str> {
    let at = dict.find(&format!("/{key} ["))? + key.len() + 3;
    let end = dict[at..].find(']')? + at;
    Some(&dict[at..end])
}

fn int_of(dict: &str, key: &str) -> Option<i64> {
    Regex::new(&format!(r"/{key} (-?\d+)\b"))
        .unwrap()
        .captures(dict)
        .map(|c| c[1].parse().unwrap())
}

/// The page objects in order.
fn page_ids(objs: &BTreeMap<u32, Obj>) -> Vec<u32> {
    let catalog = objs
        .values()
        .find(|o| o.dict.contains("/Type /Catalog"))
        .expect("a catalog");
    let pages = &objs[&ref_of(&catalog.dict, "Pages").unwrap()].dict;
    ref_list(array_of(pages, "Kids").expect("kids"))
}

#[test]
fn ec9_the_file_is_a_pdf_with_a_valid_cross_reference_table() {
    let (_d, r, _ids) = project();
    let (pdf, ..) = pdf_of(&canon(&r));
    assert!(pdf.starts_with(b"%PDF-1.7\n"), "{:?}", &pdf[..9]);
    assert!(pdf.ends_with(b"%%EOF"));
    let at = startxref(&pdf);
    assert!(pdf[at..].starts_with(b"xref\n"), "startxref points at xref");
    let table = xref(&pdf);
    assert!(table.len() > 100, "{} objects", table.len());
    // each entry points at its own `<n> 0 obj`
    let objs = objects(&pdf);
    assert_eq!(objs.len(), table.len());
    // every reference names an object of the table
    for (id, o) in &objs {
        for r in ref_list(&o.dict) {
            assert!(
                table.contains_key(&r),
                "object {id} names {r}, which is not in the table"
            );
        }
    }
    // the trailer names the catalog and the document info
    let trailer = String::from_utf8_lossy(&pdf[find(&pdf, b"trailer", at).unwrap()..]).into_owned();
    let root = ref_of(&trailer, "Root").expect("a root");
    assert!(objs[&root].dict.contains("/Type /Catalog"));
    let info = &objs[&ref_of(&trailer, "Info").expect("an info")].dict;
    assert!(
        info.contains(&format!("/Producer (rdy {})", env!("CARGO_PKG_VERSION"))),
        "{info}"
    );
    assert!(info.contains("/Title "), "{info}");
    // no date: the bytes depend on the state only (O15)
    assert!(!info.contains("Date"), "{info}");
    assert_eq!(
        int_of(&trailer, "Size"),
        Some(i64::from(table.keys().max().unwrap() + 1))
    );
}

#[test]
fn ec9_each_page_of_the_layout_is_a_page() {
    let (_d, r, _ids) = project();
    let p = canon(&r);
    let laid = laid_of(&p);
    let (pdf, pages, _) = pdf_of(&p);
    assert_eq!(pages, laid.pages.len());
    assert!(pages > 60, "the title part and each doc: {pages}");
    let objs = objects(&pdf);
    let ids = page_ids(&objs);
    assert_eq!(ids.len(), pages);
    let page_objects = objs
        .values()
        .filter(|o| Regex::new(r"/Type /Page\b").unwrap().is_match(&o.dict))
        .count();
    assert_eq!(page_objects, pages);
    let catalog = objs
        .values()
        .find(|o| o.dict.contains("/Type /Catalog"))
        .unwrap();
    let tree = &objs[&ref_of(&catalog.dict, "Pages").unwrap()].dict;
    assert_eq!(int_of(tree, "Count"), Some(pages as i64));
    for id in &ids {
        let d = &objs[id].dict;
        assert!(d.contains("/MediaBox [0 0 595.28 841.89]"), "A4 (O5): {d}");
        let c = ref_of(d, "Contents").expect("a content stream");
        let content = &objs[&c];
        assert!(
            content.dict.contains("/Filter /FlateDecode"),
            "{}",
            content.dict
        );
        assert!(content.stream.as_ref().is_some_and(|s| !s.is_empty()));
    }
    // the outline: one entry per layout outline entry
    let outlines = &objs[&ref_of(&catalog.dict, "Outlines").expect("an outline")].dict;
    assert!(outlines.contains("/Type /Outlines"));
    let items = objs
        .values()
        .filter(|o| o.dict.contains("/Title ") && o.dict.contains("/Parent "))
        .count();
    assert_eq!(items, laid.outline.len());
    assert!(items > pages, "the docs and their headings");
}

#[test]
fn ec9_fonts_are_embedded_subsets_with_to_unicode() {
    let (_d, r, _ids) = project();
    let (pdf, ..) = pdf_of(&canon(&r));
    let objs = objects(&pdf);
    let type0: Vec<&Obj> = objs
        .values()
        .filter(|o| o.dict.contains("/Subtype /Type0"))
        .collect();
    let subset = Regex::new(r"/BaseFont /([A-Z]{6})\+(DejaVuSans(-Bold)?)\b").unwrap();
    let mut names = BTreeSet::new();
    for f in &type0 {
        let c = subset
            .captures(&f.dict)
            .unwrap_or_else(|| panic!("a subset tag: {}", f.dict));
        names.insert(c[2].to_string());
        assert!(f.dict.contains("/Encoding /Identity-H"), "{}", f.dict);
        let tu = &objs[&ref_of(&f.dict, "ToUnicode").expect("a ToUnicode map")];
        assert!(tu.stream.is_some(), "the map is a stream");
        let kids = ref_list(array_of(&f.dict, "DescendantFonts").expect("a CID font"));
        assert_eq!(kids.len(), 1);
        let cid = &objs[&kids[0]].dict;
        assert!(cid.contains("/Subtype /CIDFontType2"), "{cid}");
        assert!(cid.contains("/CIDToGIDMap /Identity"), "{cid}");
        assert!(cid.contains("/W ["), "{cid}");
        let desc = &objs[&ref_of(cid, "FontDescriptor").expect("a descriptor")].dict;
        let file = &objs[&ref_of(desc, "FontFile2").expect("an embedded TrueType file")];
        assert!(file.dict.contains("/Filter /FlateDecode"), "{}", file.dict);
        let full = if &c[2] == "DejaVuSans" {
            print::fonts::SANS.len()
        } else {
            print::fonts::SANS_BOLD.len()
        };
        let length1 = int_of(&file.dict, "Length1").expect("Length1") as usize;
        assert!(
            length1 < full / 4,
            "{} is a subset: {length1} of {full} bytes",
            &c[2]
        );
        assert!(file.stream.as_ref().unwrap().len() < length1);
    }
    assert_eq!(
        names,
        ["DejaVuSans", "DejaVuSans-Bold"]
            .map(String::from)
            .into_iter()
            .collect(),
        "the corpus uses both faces, each once"
    );
    assert_eq!(type0.len(), 2);
    // code is in Courier, the standard font, with WinAnsiEncoding, not embedded
    let courier: Vec<&Obj> = objs
        .values()
        .filter(|o| o.dict.contains("/BaseFont /Courier"))
        .collect();
    assert_eq!(courier.len(), 1);
    assert!(courier[0].dict.contains("/Subtype /Type1"));
    assert!(courier[0].dict.contains("/Encoding /WinAnsiEncoding"));
    assert!(ref_of(&courier[0].dict, "FontDescriptor").is_none());
}

#[test]
fn ec9_internal_links_go_to_their_anchor_page() {
    let (_d, r, _ids) = project();
    let p = canon(&r);
    let laid = laid_of(&p);
    let (pdf, ..) = pdf_of(&p);
    let objs = objects(&pdf);
    let ids = page_ids(&objs);
    let num = r"(-?[0-9.]+)";
    let rect_re = Regex::new(&format!(r"/Rect \[{num} {num} {num} {num}\]")).unwrap();
    let goto_re = Regex::new(&format!(
        r"/S /GoTo\s+/D \[(\d+) 0 R /XYZ {num} {num} {num}\]"
    ))
    .unwrap();
    let uri_re = Regex::new(r"/S /URI\s+/URI \(").unwrap();
    let (mut internal, mut with_anchor) = (0, 0);
    for (i, page) in laid.pages.iter().enumerate() {
        let links: Vec<(&[f32; 4], &Href)> = page
            .items
            .iter()
            .filter_map(|it| match it {
                LItem::Link { rect, href } => Some((rect, href)),
                _ => None,
            })
            .collect();
        let annots: Vec<u32> = array_of(&objs[&ids[i]].dict, "Annots")
            .map(ref_list)
            .unwrap_or_default();
        assert_eq!(
            annots.len(),
            links.len(),
            "page {i}: one annotation per link"
        );
        for ((rect, href), a) in links.into_iter().zip(annots) {
            let d = &objs[&a].dict;
            assert!(d.contains("/Subtype /Link"), "{d}");
            let rc = rect_re.captures(d).expect("a rect");
            for k in 0..4 {
                let v: f32 = rc[k + 1].parse().unwrap();
                assert!((v - rect[k]).abs() < 1e-3, "{d} vs {rect:?}");
            }
            match href {
                Href::Page { page, anchor } => {
                    let g = goto_re.captures(d).unwrap_or_else(|| panic!("a GoTo: {d}"));
                    let want = anchor
                        .as_ref()
                        .and_then(|a| laid.anchors.get(&(page.clone(), Some(a.clone()))))
                        .or_else(|| laid.anchors.get(&(page.clone(), None)))
                        .expect("the target is in the layout");
                    if anchor.is_some() {
                        with_anchor += 1;
                    }
                    let target: u32 = g[1].parse().unwrap();
                    assert_eq!(target, ids[want.0], "{page} {anchor:?}");
                    let y: f32 = g[3].parse().unwrap();
                    assert!((y - want.1).abs() < 1e-3, "{y} vs {}", want.1);
                    internal += 1;
                }
                Href::External(u) => {
                    assert!(uri_re.is_match(d), "{d}");
                    assert!(!u.is_empty());
                }
            }
        }
    }
    assert!(internal > 10, "the corpus links to pages: {internal}");
    assert!(with_anchor > 5, "links to anchors: {with_anchor}");
}

/// CRC-32 (ISO 3309), as PNG's chunks carry it.
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

/// An 8-bit RGBA PNG of `w` × `h`, its scanlines unfiltered and stored with no compression.
fn rgba_png(w: u32, h: u32) -> Vec<u8> {
    let mut raw = vec![];
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            raw.extend_from_slice(&[
                (x * 80) as u8,
                (y * 120) as u8,
                200,
                (x * 60 + y * 30) as u8,
            ]);
        }
    }
    let mut z = vec![0x78, 0x01, 0x01];
    let len = raw.len() as u16;
    z.extend_from_slice(&len.to_le_bytes());
    z.extend_from_slice(&(!len).to_le_bytes());
    z.extend_from_slice(&raw);
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = vec![];
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// The corpus with a figure of `bytes` as `media_type`, landed.
fn with_figure(media_type: &str, bytes: &[u8], alt: &str) -> (tempfile::TempDir, Rdy) {
    let (d, mut r, ids) = project();
    let blob = r.store_mut().put_blob(bytes).unwrap().to_string();
    write(
        &mut r,
        "picture",
        json!([{"op": "create", "type": "figure", "parent": ids.guards_section.to_string(),
            "fields": {"blob": blob, "media_type": media_type, "alt": alt}}]),
    )
    .expect("a figure");
    land(&mut r, "picture").expect("land it");
    (d, r)
}

#[test]
fn ec9_an_rgba_png_prints_with_its_alpha_as_a_soft_mask() {
    let (_d, r) = with_figure("image/png", &rgba_png(5, 3), "A see-through square");
    let p = canon(&r);
    let laid = laid_of(&p);
    let (pdf, _, warnings) = pdf_of(&p);
    assert!(
        !warnings.iter().any(|w| w.contains("A see-through square")),
        "{warnings:?}"
    );
    // the layout draws the figure as an image
    let drawn: Vec<(usize, f32, f32)> = laid
        .pages
        .iter()
        .enumerate()
        .flat_map(|(i, pg)| {
            pg.items.iter().filter_map(move |it| match it {
                LItem::Image { w, h, .. } => Some((i, *w, *h)),
                _ => None,
            })
        })
        .collect();
    // the corpus's own PNG figure, and this one at its aspect
    assert_eq!(drawn.len(), 2, "{drawn:?}");
    let square = drawn
        .iter()
        .find(|d| (d.1 / d.2 - 5.0 / 3.0).abs() < 1e-3)
        .expect("the figure at 5:3");
    let objs = objects(&pdf);
    let (id, color) = objs
        .iter()
        .find(|(_, o)| {
            o.dict.contains("/Subtype /Image")
                && o.dict.contains("/Width 5")
                && o.dict.contains("/Height 3")
                && o.dict.contains("/SMask ")
        })
        .expect("the image, with a soft mask");
    let d = &color.dict;
    assert!(d.contains("/ColorSpace /DeviceRGB"), "{d}");
    assert!(d.contains("/BitsPerComponent 8"), "{d}");
    assert!(d.contains("/Filter /FlateDecode"), "{d}");
    let mask = &objs[&ref_of(&color.dict, "SMask").unwrap()].dict;
    assert!(mask.contains("/Subtype /Image"), "{mask}");
    assert!(
        mask.contains("/Width 5") && mask.contains("/Height 3"),
        "{mask}"
    );
    assert!(mask.contains("/ColorSpace /DeviceGray"), "{mask}");
    assert!(mask.contains("/BitsPerComponent 8"), "{mask}");
    assert!(mask.contains("/Filter /FlateDecode"), "{mask}");
    // the page that draws it names it in its resources
    let ids = page_ids(&objs);
    let page = &objs[&ids[square.0]].dict;
    assert!(page.contains(&format!(" {id} 0 R")), "{page}");
}

#[test]
fn ec9_an_image_type_that_does_not_print_leaves_a_line_and_a_warning() {
    // the alt has a character with no glyph, so the glyph warning comes last
    let (_d, r) = with_figure("image/gif", b"GIF89a....", "A moving picture 中");
    let p = canon(&r);
    let laid = laid_of(&p);
    let (pdf, _, warnings) = pdf_of(&p);
    let mut want = p.warnings.clone();
    want.push(
        "the figure \"A moving picture 中\" is a image/gif image, which does not print in a PDF; \
         the HTML and Markdown exports show it"
            .to_string(),
    );
    want.push("1 characters have no glyph in the PDF fonts and print as ?: 中".to_string());
    // the order: the build's warnings, then each figure in print order, then the glyphs
    assert_eq!(warnings, want);
    // the line in its place
    let line =
        laid.pages.iter().flat_map(|pg| &pg.items).any(
            |it| matches!(it, LItem::Text { text, .. } if text == "Figure: A moving picture ?"),
        );
    assert!(line, "the figure leaves a line");
    // the file holds only the corpus's own PNG figure
    let drawn = laid
        .pages
        .iter()
        .flat_map(|pg| &pg.items)
        .filter(|it| matches!(it, LItem::Image { .. }))
        .count();
    assert_eq!(drawn, 1);
    let objs = objects(&pdf);
    let gif = p
        .blobs
        .values()
        .find(|b| b.media_type == "image/gif")
        .expect("the print holds the GIF's blob");
    assert_eq!(gif.bytes, b"GIF89a....");
    assert!(
        !objs
            .values()
            .any(|o| o.stream.as_deref() == Some(&gif.bytes[..])),
        "the GIF's bytes are not in the file"
    );
}

#[test]
fn ec9_two_exports_are_the_same_bytes() {
    let (_d, r) = with_figure("image/png", &rgba_png(4, 4), "A square");
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a.pdf"), dir.path().join("b.pdf"));
    let sa = print::pdf::write_pdf(&canon(&r), &a).unwrap();
    let sb = print::pdf::write_pdf(&canon(&r), &b).unwrap();
    let bytes = std::fs::read(&a).unwrap();
    assert_eq!(bytes, std::fs::read(&b).unwrap());
    assert_eq!(bytes, pdf_of(&canon(&r)).0);
    let view = print::build(&r.read(), &Scope::View("editing".into())).unwrap();
    assert_eq!(pdf_of(&view).0, pdf_of(&view).0);
    // the summary
    let p = canon(&r);
    let (_, pages, warnings) = pdf_of(&p);
    assert_eq!(sa.format, "pdf");
    assert_eq!(sa.out, a.display().to_string());
    assert_eq!(sa.bytes, bytes.len() as u64);
    assert_eq!(sa.pages, Some(pages));
    assert_eq!(sa.warnings, warnings);
    assert_eq!(
        sa.assets, 2,
        "the images the file holds: the corpus's and this one"
    );
    assert_eq!(
        sa.docs,
        p.docs
            .iter()
            .filter(|d| d.kind == print::PageKind::Doc)
            .count()
    );
    assert_eq!(
        sa.folders,
        p.docs
            .iter()
            .filter(|d| d.kind == print::PageKind::Folder)
            .count()
    );
    assert_eq!(sb.bytes, sa.bytes);
}

#[test]
fn ec9_an_existing_out_is_refused() {
    let (d, r, _ids) = project();
    let p = canon(&r);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("p.pdf");
    std::fs::write(&out, b"mine").unwrap();
    let e = print::pdf::write_pdf(&p, &out).unwrap_err();
    assert_eq!(e.code, E_ALREADY_EXISTS);
    assert_eq!(
        e.message,
        format!(
            "{} exists; rdy export never overwrites a file",
            out.display()
        )
    );
    assert_eq!(e.fix, "pick another path, or remove the file");
    assert_eq!(
        std::fs::read(&out).unwrap(),
        b"mine",
        "the file is unchanged"
    );
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(left.len(), 1, "no partial file is left");
    // a directory is no file to place either
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    assert_eq!(
        print::pdf::write_pdf(&p, &sub).unwrap_err().code,
        E_ALREADY_EXISTS
    );
    // O29: a parent that does not exist, and a path in the project's .rdy directory
    let e = print::pdf::write_pdf(&p, &dir.path().join("none/p.pdf")).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    let inside = d.path().join(".rdy").join("p.pdf");
    let e = print::pdf::write_pdf(&p, &inside).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        format!("{} is inside the project .rdy directory", inside.display())
    );
    assert!(std::fs::symlink_metadata(&inside).is_err());
    // then the file is placed whole where nothing was
    let free = dir.path().join("free.pdf");
    print::pdf::write_pdf(&p, &free).unwrap();
    assert_eq!(std::fs::read(&free).unwrap(), pdf_of(&p).0);
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.contains(".partial-")), "{names:?}");
}
