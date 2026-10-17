//! The PDF's page layout (D-WQSAY, D-EVRNM; O5): a print set on A4 pages as positioned text,
//! rules, images and link areas, which the PDF writer draws as they are.
//!
//! Every place is in PDF user space: points from the bottom-left corner of the page, `y` up. A
//! `Text` item's `x` and `y` are where its baseline starts. An `Image` item's `x` and `y` are its
//! bottom-left corner. A `Link` rect is `[left, bottom, right, top]`. An anchor's and an outline
//! entry's `y` is the top of its block.
//!
//! The rules are closed: body Sans 10.5 pt on 14 pt lines; headings in Bold at 20, 16, 13 and
//! 11.5 pt, with 1.2 times the size before; greedy line breaks at spaces, and between characters
//! for a word wider than the line; lists indented 14 pt a level with `•` or `n.` markers; tables
//! with equal column widths and a Bold head row, where a row splits only when it is taller than a
//! page; code in Courier 9 pt on 11 pt lines, wrapped at the width; figures at the text width and
//! at most 60% of the page height, the caption below in Italic; callouts indented 10 pt with a
//! 2 pt rule at the left and the label in Bold. The title part, each top-level folder and each doc
//! start a new page. Each page has its number centred 10 mm from the bottom. A character with no
//! glyph (`Fonts::glyph`, which gives none for a script that needs shaping, RULING F1) prints as
//! `?` and is reported in [`Laid::missing`], before any measuring. The spacing the rules do not
//! name is the constants below (O32).
use super::fonts::{Face, Fonts};
use super::{Href, PBlock, PageKind, Print, PrintDoc, Run};
use rdy_model::Hash;
use std::collections::{BTreeMap, BTreeSet};

/// The paper, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageSpec {
    pub width: f32,
    pub height: f32,
    pub margin: f32,
}

impl PageSpec {
    /// A4 portrait with 20 mm margins (O5).
    pub const A4: PageSpec = PageSpec {
        width: 595.28,
        height: 841.89,
        margin: 56.69,
    };
}

/// One thing drawn on a page.
#[derive(Clone, Debug, PartialEq)]
pub enum LItem {
    /// One line's run of text in one face and size; a character with no glyph is already `?`.
    Text {
        x: f32,
        y: f32,
        face: Face,
        size: f32,
        text: String,
    },
    Rule {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        width: f32,
    },
    Image {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        blob: Hash,
    },
    Link {
        rect: [f32; 4],
        href: Href,
    },
}

/// One page: what it draws, and its number (the first page is 1).
#[derive(Clone, Debug, PartialEq)]
pub struct LPage {
    pub items: Vec<LItem>,
    pub number: usize,
}

/// A bookmark: a print doc (level 0) or a heading (its level).
#[derive(Clone, Debug, PartialEq)]
pub struct OutlineEntry {
    pub title: String,
    pub level: u8,
    pub page: usize,
    pub y: f32,
}

/// A laid-out print.
#[derive(Clone, Debug, PartialEq)]
pub struct Laid {
    pub pages: Vec<LPage>,
    /// (page path, anchor) → (page index, y): a print doc's start under `None`, each block
    /// anchor under its name.
    pub anchors: BTreeMap<(String, Option<String>), (usize, f32)>,
    pub outline: Vec<OutlineEntry>,
    /// The characters that have no glyph in their face; each prints as `?`.
    pub missing: BTreeSet<char>,
}

/// Body text: size and line height.
const BODY_SIZE: f32 = 10.5;
const BODY_LINE: f32 = 14.0;
/// Heading sizes by level, 1 to 6.
const HEADING_SIZES: [f32; 6] = [20.0, 16.0, 13.0, 11.5, 11.5, 11.5];
/// A heading's line height, as a multiple of its size.
const HEADING_LINE: f32 = 1.25;
/// The space before a heading, as a multiple of its size.
const HEADING_BEFORE: f32 = 1.2;
/// Code: size and line height.
const CODE_SIZE: f32 = 9.0;
const CODE_LINE: f32 = 11.0;
/// A tab in code moves to the next multiple of this many columns.
const TAB_STOP: usize = 4;
/// Lists: indent per level, and the least space between a marker and its item. A marker
/// ends that space before its item; one too wide for the indent is set smaller to fit.
const LIST_INDENT: f32 = 14.0;
const MARKER_GAP: f32 = 3.0;
/// Callouts: indent and the width of the rule at the left.
const CALLOUT_INDENT: f32 = 10.0;
const CALLOUT_RULE: f32 = 2.0;
/// A figure's greatest height, as a share of the page height.
const FIGURE_MAX: f32 = 0.6;
/// The page number: its size and its baseline (10 mm from the bottom).
const PAGE_NUMBER_SIZE: f32 = 9.0;
const PAGE_NUMBER_Y: f32 = 10.0 / 25.4 * 72.0;
/// The gap between two blocks, between two list items, and before a caption.
const BLOCK_GAP: f32 = 6.0;
const ITEM_GAP: f32 = 2.0;
const CAPTION_GAP: f32 = 4.0;
/// The gap before a nested folder that follows on the page.
const SECTION_GAP: f32 = 18.0;
/// Tables: the padding inside a cell and the width of the rules between rows.
const CELL_PAD: f32 = 3.0;
const TABLE_RULE: f32 = 0.5;
/// The title part: how far below the top of the text area it starts, the name's size and line
/// height, the description's, and the gap before the version and scope lines.
const TITLE_DROP: f32 = 160.0;
const TITLE_SIZE: f32 = 28.0;
const TITLE_LINE: f32 = 34.0;
const SUBTITLE_SIZE: f32 = 13.0;
const SUBTITLE_LINE: f32 = 18.0;
const TITLE_GAP: f32 = 24.0;

/// How a character looks: its face, its size and the link it is part of.
#[derive(Clone, Debug, PartialEq)]
struct Style {
    face: Face,
    size: f32,
    link: Option<Href>,
}

/// Text to set: each character with the index of its style.
#[derive(Default)]
struct Styled {
    chars: Vec<(char, usize)>,
    styles: Vec<Style>,
}

impl Styled {
    fn of(text: &str, face: Face, size: f32) -> Styled {
        let mut s = Styled::default();
        s.push(
            text,
            Style {
                face,
                size,
                link: None,
            },
        );
        s
    }

    /// `runs` in `size`; `bold` and `italic` apply to every run (a heading, a head cell, a
    /// caption).
    fn runs(runs: &[Run], bold: bool, italic: bool, size: f32) -> Styled {
        let mut s = Styled::default();
        for r in runs {
            let face = match (r.code, r.bold || bold, r.italic || italic) {
                (true, ..) => Face::Mono,
                (false, true, true) => Face::BoldItalic,
                (false, true, false) => Face::Bold,
                (false, false, true) => Face::Italic,
                (false, false, false) => Face::Sans,
            };
            s.push(
                &r.text,
                Style {
                    face,
                    size,
                    link: r.link.clone(),
                },
            );
        }
        s
    }

    fn push(&mut self, text: &str, style: Style) {
        let i = match self.styles.iter().position(|s| *s == style) {
            Some(i) => i,
            None => {
                self.styles.push(style);
                self.styles.len() - 1
            }
        };
        self.chars.extend(text.chars().map(|c| (c, i)));
    }
}

/// The words of `runs`, for the outline.
fn plain(runs: &[Run]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}

/// `line` with each tab moved on to the next tab stop.
fn expand_tabs(line: &str) -> String {
    let mut out = String::new();
    let mut col = 0;
    for c in line.chars() {
        if c == '\t' {
            let n = TAB_STOP - col % TAB_STOP;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

/// The pages being set, and where the next line goes.
struct Layout<'a> {
    fonts: &'a Fonts,
    spec: &'a PageSpec,
    image_size: &'a dyn Fn(&Hash) -> Option<(u32, u32)>,
    pages: Vec<Vec<LItem>>,
    /// The top of the next line on the last page.
    y: f32,
    /// List markers that wait for the first line of their item: (x, size, text).
    markers: Vec<(f32, f32, String)>,
    anchors: BTreeMap<(String, Option<String>), (usize, f32)>,
    outline: Vec<OutlineEntry>,
    missing: BTreeSet<char>,
}

impl Layout<'_> {
    fn top(&self) -> f32 {
        self.spec.height - self.spec.margin
    }

    fn bottom(&self) -> f32 {
        self.spec.margin
    }

    fn text_width(&self) -> f32 {
        self.spec.width - 2.0 * self.spec.margin
    }

    fn new_page(&mut self) {
        self.pages.push(vec![]);
        self.y = self.top();
    }

    fn at_top(&self) -> bool {
        self.y >= self.top()
    }

    fn here(&self) -> (usize, f32) {
        (self.pages.len() - 1, self.y)
    }

    fn push(&mut self, item: LItem) {
        if let Some(page) = self.pages.last_mut() {
            page.push(item);
        }
    }

    /// Space before a block; none at the top of a page.
    fn gap(&mut self, g: f32) {
        if !self.at_top() {
            self.y -= g;
        }
    }

    /// A new page when `h` does not fit above the bottom margin.
    fn ensure(&mut self, h: f32) {
        if !self.at_top() && self.y - h < self.bottom() {
            self.new_page();
        }
    }

    /// Records `anchor` of `page` here; the first place an anchor is met wins.
    fn anchor(&mut self, page: &str, anchor: Option<&str>) {
        let here = self.here();
        self.anchors
            .entry((page.to_string(), anchor.map(str::to_string)))
            .or_insert(here);
    }

    fn block_anchor(&mut self, page: &str, anchor: &Option<String>) {
        if let Some(a) = anchor {
            self.anchor(page, Some(a));
        }
    }

    /// The baseline of a line of `size` text whose box of `line_h` starts at the cursor.
    fn baseline(&self, line_h: f32, size: f32) -> f32 {
        let asc = self.fonts.ascent(Face::Sans, size);
        let desc = self.fonts.descent(Face::Sans, size);
        self.y - (line_h - (asc - desc)) / 2.0 - asc
    }

    fn advance(&self, s: &Styled, (c, st): (char, usize)) -> f32 {
        let style = &s.styles[st];
        self.fonts.advance(style.face, c, style.size)
    }

    /// The waiting list markers, on the line whose box starts at the cursor.
    fn flush_markers(&mut self) {
        let baseline = self.baseline(BODY_LINE, BODY_SIZE);
        self.place_markers(baseline);
    }

    /// The waiting list markers, on `baseline`.
    fn place_markers(&mut self, baseline: f32) {
        for (x, size, text) in std::mem::take(&mut self.markers) {
            let text = self.printable(Face::Sans, &text);
            self.push(LItem::Text {
                x,
                y: baseline,
                face: Face::Sans,
                size,
                text,
            });
        }
    }

    /// A list marker for an item whose text starts at `x + LIST_INDENT`: its place and size.
    /// It ends `MARKER_GAP` before the item, and it is set smaller when it is wider than the
    /// space the indent leaves, so that it starts no further left than `x`.
    fn marker(&self, x: f32, text: &str) -> (f32, f32) {
        let room = LIST_INDENT - MARKER_GAP;
        let w: f32 = text
            .chars()
            .map(|c| self.fonts.advance(Face::Sans, c, BODY_SIZE))
            .sum();
        let (size, w) = if w > room {
            (BODY_SIZE * room / w, room)
        } else {
            (BODY_SIZE, w)
        };
        (x + room - w, size)
    }

    /// `text` with each character `face` has no glyph for as `?`, recorded in `missing`.
    fn printable(&mut self, face: Face, text: &str) -> String {
        text.chars()
            .map(|c| {
                if self.fonts.glyph(face, c).is_some() {
                    c
                } else {
                    self.missing.insert(c);
                    '?'
                }
            })
            .collect()
    }

    /// Greedy lines of `s` at most `width` wide, as ranges of its characters: a line breaks at
    /// a space, which it drops, or at a `\n`; a word wider than the line breaks between
    /// characters.
    fn break_lines(&self, s: &Styled, width: f32) -> Vec<(usize, usize)> {
        let chars = &s.chars;
        let mut lines = vec![];
        let (mut start, mut w, mut last_space) = (0, 0.0, None);
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i].0;
            if c == '\n' {
                lines.push((start, i));
                (start, w, last_space) = (i + 1, 0.0, None);
                i += 1;
                continue;
            }
            if c == ' ' && i == start {
                // a line never starts with a space
                start += 1;
                i += 1;
                continue;
            }
            let a = self.advance(s, chars[i]);
            if c != ' ' && i > start && w + a > width {
                match last_space {
                    Some(sp) => {
                        lines.push((start, sp));
                        start = sp + 1;
                        w = (start..i).map(|j| self.advance(s, chars[j])).sum();
                        last_space = None;
                    }
                    None => {
                        lines.push((start, i));
                        (start, w) = (i, 0.0);
                    }
                }
                // the same character again, on the new line
                continue;
            }
            if c == ' ' {
                last_space = Some(i);
            }
            w += a;
            i += 1;
        }
        lines.push((start, chars.len()));
        lines
    }

    /// The characters `range` of `s` from `x` on `baseline`, one text item per style. With
    /// `trim`, the spaces at either end of the line and of each item are left out (they still
    /// take their place).
    fn place_line(&mut self, x: f32, baseline: f32, s: &Styled, range: (usize, usize), trim: bool) {
        let mut chars = &s.chars[range.0..range.1];
        if trim {
            while let [(' ', _), rest @ ..] = chars {
                chars = rest;
            }
            while let [rest @ .., (' ', _)] = chars {
                chars = rest;
            }
        }
        let mut x = x;
        let mut i = 0;
        while i < chars.len() {
            let st = chars[i].1;
            let j = chars[i..]
                .iter()
                .position(|(_, s)| *s != st)
                .map_or(chars.len(), |n| i + n);
            let group = &chars[i..j];
            let mut lead = 0.0;
            let mut body = group;
            if trim {
                while let [first @ (' ', _), rest @ ..] = body {
                    lead += self.advance(s, *first);
                    body = rest;
                }
                while let [rest @ .., (' ', _)] = body {
                    body = rest;
                }
            }
            let width: f32 = group.iter().map(|c| self.advance(s, *c)).sum();
            let body_width: f32 = body.iter().map(|c| self.advance(s, *c)).sum();
            let style = s.styles[st].clone();
            let raw: String = body.iter().map(|(c, _)| *c).collect();
            let text = self.printable(style.face, &raw);
            if !text.is_empty() {
                let tx = x + lead;
                if let Some(href) = style.link {
                    let rect = [
                        tx,
                        baseline + self.fonts.descent(style.face, style.size),
                        tx + body_width,
                        baseline + self.fonts.ascent(style.face, style.size),
                    ];
                    self.push(LItem::Link { rect, href });
                }
                self.push(LItem::Text {
                    x: tx,
                    y: baseline,
                    face: style.face,
                    size: style.size,
                    text,
                });
            }
            x += width;
            i = j;
        }
    }

    /// One line of `line_h` at the cursor, on a new page when it does not fit; the waiting list
    /// markers go on it.
    fn line(&mut self, x: f32, s: &Styled, range: (usize, usize), line_h: f32, trim: bool) {
        self.ensure(line_h);
        let size = s.chars[range.0..range.1]
            .iter()
            .map(|(_, st)| s.styles[*st].size)
            .fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.max(v))))
            .unwrap_or(BODY_SIZE);
        let baseline = self.baseline(line_h, size);
        self.place_markers(baseline);
        self.place_line(x, baseline, s, range, trim);
        self.y -= line_h;
    }

    /// `s` wrapped at `w` from `x`, in lines of `line_h`.
    fn flow(&mut self, x: f32, w: f32, s: &Styled, line_h: f32) {
        if s.chars.is_empty() {
            return;
        }
        for range in self.break_lines(s, w) {
            self.line(x, s, range, line_h, true);
        }
    }
}

impl Layout<'_> {
    /// The title part: the project's name, its description, the version label and the scope.
    fn title_part(&mut self, p: &Print) {
        self.new_page();
        self.y = self.top() - TITLE_DROP;
        let w = self.text_width();
        let x = self.spec.margin;
        self.flow(
            x,
            w,
            &Styled::of(&p.title, Face::Bold, TITLE_SIZE),
            TITLE_LINE,
        );
        if let Some(d) = &p.description {
            self.y -= BLOCK_GAP;
            self.flow(
                x,
                w,
                &Styled::of(d, Face::Sans, SUBTITLE_SIZE),
                SUBTITLE_LINE,
            );
        }
        self.y -= TITLE_GAP;
        for line in [
            format!("Version: {}", p.label),
            format!("Scope: {}", p.scope),
        ] {
            self.flow(x, w, &Styled::of(&line, Face::Sans, BODY_SIZE), BODY_LINE);
        }
    }

    /// A print doc: a new page for a doc, the glossary and a top-level folder; its title as a
    /// level-1 heading; then its blocks.
    fn doc(&mut self, d: &PrintDoc) {
        let starts_page = d.kind != PageKind::Folder || d.depth == 0;
        if starts_page {
            self.new_page();
        } else {
            self.gap(SECTION_GAP);
        }
        let size = HEADING_SIZES[0];
        let line_h = size * HEADING_LINE;
        self.ensure(line_h + BODY_LINE);
        self.anchor(&d.page, None);
        let (page, y) = self.here();
        self.outline.push(OutlineEntry {
            title: d.title.clone(),
            level: 0,
            page,
            y,
        });
        let (x, w) = (self.spec.margin, self.text_width());
        self.flow(x, w, &Styled::of(&d.title, Face::Bold, size), line_h);
        self.blocks(&d.page, &d.blocks, x, w, BLOCK_GAP);
    }

    /// `blocks` from `x`, `w` wide; `first_gap` before the first.
    fn blocks(&mut self, page: &str, blocks: &[PBlock], x: f32, w: f32, first_gap: f32) {
        for (i, b) in blocks.iter().enumerate() {
            self.block(page, b, x, w, if i == 0 { first_gap } else { BLOCK_GAP });
        }
    }

    fn block(&mut self, page: &str, b: &PBlock, x: f32, w: f32, gap: f32) {
        match b {
            PBlock::Para { anchor, runs } => {
                self.gap(gap);
                self.ensure(BODY_LINE);
                self.block_anchor(page, anchor);
                self.flow(
                    x,
                    w,
                    &Styled::runs(runs, false, false, BODY_SIZE),
                    BODY_LINE,
                );
            }
            PBlock::Heading {
                level,
                anchor,
                runs,
            } => {
                let size = HEADING_SIZES[usize::from(level.clamp(&1, &6) - 1)];
                let line_h = size * HEADING_LINE;
                self.gap(gap.max(HEADING_BEFORE * size));
                // a heading keeps a line of what follows it
                self.ensure(line_h + BODY_LINE);
                self.block_anchor(page, anchor);
                let (pg, y) = self.here();
                self.outline.push(OutlineEntry {
                    title: plain(runs),
                    level: *level,
                    page: pg,
                    y,
                });
                self.flow(x, w, &Styled::runs(runs, true, false, size), line_h);
            }
            PBlock::List {
                ordered,
                start,
                items,
            } => {
                for (i, item) in items.iter().enumerate() {
                    let marker = if *ordered {
                        let n = u64::from(*start) + u64::try_from(i).unwrap_or(u64::MAX);
                        format!("{n}.")
                    } else {
                        "•".to_string()
                    };
                    let (mx, size) = self.marker(x, &marker);
                    self.markers.push((mx, size, marker));
                    let g = if i == 0 { gap } else { ITEM_GAP };
                    if item.is_empty() {
                        self.gap(g);
                        self.line(x, &Styled::default(), (0, 0), BODY_LINE, true);
                    } else {
                        self.blocks(page, item, x + LIST_INDENT, w - LIST_INDENT, g);
                        // an item with no line of text: its marker at the end of the item
                        self.flush_markers();
                    }
                }
            }
            PBlock::Table { anchor, head, rows } => self.table(page, anchor, head, rows, x, w, gap),
            PBlock::Code { anchor, text, .. } => {
                self.gap(gap);
                self.ensure(CODE_LINE);
                self.block_anchor(page, anchor);
                let text = text.strip_suffix('\n').unwrap_or(text);
                for src in text.split('\n') {
                    let s = Styled::of(&expand_tabs(src), Face::Mono, CODE_SIZE);
                    // wrapped at the width, between any two characters, with no hyphen
                    let mut start = 0;
                    let mut used = 0.0;
                    for i in 0..s.chars.len() {
                        let a = self.advance(&s, s.chars[i]);
                        if i > start && used + a > w {
                            self.line(x, &s, (start, i), CODE_LINE, false);
                            (start, used) = (i, 0.0);
                        }
                        used += a;
                    }
                    self.line(x, &s, (start, s.chars.len()), CODE_LINE, false);
                }
            }
            PBlock::Figure {
                anchor,
                blob,
                alt,
                caption,
            } => {
                self.gap(gap);
                let sized = blob.and_then(|h| {
                    (self.image_size)(&h)
                        .filter(|(pw, ph)| *pw > 0 && *ph > 0)
                        .map(|s| (h, s))
                });
                match sized {
                    Some((blob, (pw, ph))) => {
                        // at the text width, at most FIGURE_MAX of the page height
                        let (pw, ph) = (pw as f32, ph as f32);
                        let (mut iw, mut ih) = (w, w * ph / pw);
                        let most = FIGURE_MAX * self.spec.height;
                        if ih > most {
                            (iw, ih) = (most * pw / ph, most);
                        }
                        self.ensure(ih);
                        self.block_anchor(page, anchor);
                        self.flush_markers();
                        self.push(LItem::Image {
                            x: x + (w - iw) / 2.0,
                            y: self.y - ih,
                            w: iw,
                            h: ih,
                            blob,
                        });
                        self.y -= ih;
                    }
                    None => {
                        self.ensure(BODY_LINE);
                        self.block_anchor(page, anchor);
                        let s = Styled::of(&format!("Figure: {alt}"), Face::Italic, BODY_SIZE);
                        self.flow(x, w, &s, BODY_LINE);
                    }
                }
                if !caption.is_empty() {
                    self.gap(CAPTION_GAP);
                    self.flow(
                        x,
                        w,
                        &Styled::runs(caption, false, true, BODY_SIZE),
                        BODY_LINE,
                    );
                }
            }
            PBlock::Callout {
                anchor,
                label,
                blocks,
            } => {
                self.gap(gap);
                self.ensure(BODY_LINE);
                self.block_anchor(page, anchor);
                let start = self.here();
                let (ix, iw) = (x + CALLOUT_INDENT, w - CALLOUT_INDENT);
                let mut first_gap = 0.0;
                if !label.is_empty() {
                    self.flow(ix, iw, &Styled::of(label, Face::Bold, BODY_SIZE), BODY_LINE);
                    first_gap = ITEM_GAP;
                }
                self.blocks(page, blocks, ix, iw, first_gap);
                // the rule at the left, one piece on each page the callout is on
                let end = self.here();
                let rx = x + CALLOUT_RULE / 2.0;
                let (top, bottom) = (self.top(), self.bottom());
                for p in start.0..=end.0 {
                    let y1 = if p == start.0 { start.1 } else { top };
                    let y2 = if p == end.0 { end.1 } else { bottom };
                    if y1 > y2 {
                        self.pages[p].push(LItem::Rule {
                            x1: rx,
                            y1,
                            x2: rx,
                            y2,
                            width: CALLOUT_RULE,
                        });
                    }
                }
            }
        }
    }

    /// A rule across the table at the cursor.
    fn table_rule(&mut self, x: f32, w: f32) {
        self.push(LItem::Rule {
            x1: x,
            y1: self.y,
            x2: x + w,
            y2: self.y,
            width: TABLE_RULE,
        });
    }

    /// A table: equal columns, cells wrapped, the head row in Bold; a row moves to a new page
    /// whole unless it is taller than a page.
    #[allow(clippy::too_many_arguments)]
    fn table(
        &mut self,
        page: &str,
        anchor: &Option<String>,
        head: &[Vec<Run>],
        rows: &[Vec<Vec<Run>>],
        x: f32,
        w: f32,
        gap: f32,
    ) {
        let cols = rows.iter().map(Vec::len).fold(head.len(), usize::max);
        if cols == 0 {
            return;
        }
        let cw = w / cols as f32;
        self.gap(gap);
        let head_row = (!head.is_empty()).then_some((head, true));
        let mut first = true;
        for (cells, bold) in head_row
            .into_iter()
            .chain(rows.iter().map(|r| (r.as_slice(), false)))
        {
            let set: Vec<Styled> = (0..cols)
                .map(|c| {
                    cells
                        .get(c)
                        .map(|runs| Styled::runs(runs, bold, false, BODY_SIZE))
                        .unwrap_or_default()
                })
                .collect();
            let lines: Vec<Vec<(usize, usize)>> = set
                .iter()
                .map(|s| {
                    if s.chars.is_empty() {
                        vec![]
                    } else {
                        self.break_lines(s, cw - 2.0 * CELL_PAD)
                    }
                })
                .collect();
            let n = lines.iter().map(Vec::len).max().unwrap_or(0).max(1);
            let h = n as f32 * BODY_LINE + 2.0 * CELL_PAD;
            // a row moves to a new page whole; a row taller than a page moves only when its
            // padding and first line do not fit, so its rule and anchor go with that line
            let need = if h <= self.top() - self.bottom() {
                h
            } else {
                CELL_PAD + BODY_LINE
            };
            if !self.at_top() && self.y - need < self.bottom() {
                if !first {
                    self.table_rule(x, w);
                }
                self.new_page();
            }
            if first {
                self.block_anchor(page, anchor);
                self.flush_markers();
                first = false;
            }
            self.table_rule(x, w);
            self.y -= CELL_PAD;
            for k in 0..n {
                // only a row taller than a page gets here with no room
                if self.y - BODY_LINE < self.bottom() {
                    self.new_page();
                }
                let baseline = self.baseline(BODY_LINE, BODY_SIZE);
                for (c, s) in set.iter().enumerate() {
                    if let Some(range) = lines[c].get(k) {
                        let cx = x + c as f32 * cw + CELL_PAD;
                        self.place_line(cx, baseline, s, *range, true);
                    }
                }
                self.y -= BODY_LINE;
            }
            self.y -= CELL_PAD;
        }
        self.table_rule(x, w);
    }
}

/// The print `p` set on pages of `spec`. `image_size` gives a figure blob's size in pixels, or
/// None when the blob does not print. The same print gives the same layout.
pub fn lay_out(
    p: &Print,
    fonts: &Fonts,
    spec: &PageSpec,
    image_size: &dyn Fn(&Hash) -> Option<(u32, u32)>,
) -> Laid {
    let mut l = Layout {
        fonts,
        spec,
        image_size,
        pages: vec![],
        y: 0.0,
        markers: vec![],
        anchors: BTreeMap::new(),
        outline: vec![],
        missing: BTreeSet::new(),
    };
    l.title_part(p);
    for d in &p.docs {
        l.doc(d);
    }
    let Layout {
        pages,
        anchors,
        outline,
        missing,
        ..
    } = l;
    let pages = pages
        .into_iter()
        .enumerate()
        .map(|(i, mut items)| {
            let n = (i + 1).to_string();
            let w: f32 = n
                .chars()
                .map(|c| fonts.advance(Face::Sans, c, PAGE_NUMBER_SIZE))
                .sum();
            items.push(LItem::Text {
                x: (spec.width - w) / 2.0,
                y: PAGE_NUMBER_Y,
                face: Face::Sans,
                size: PAGE_NUMBER_SIZE,
                text: n,
            });
            LPage {
                items,
                number: i + 1,
            }
        })
        .collect();
    Laid {
        pages,
        anchors,
        outline,
        missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::{Blob, PBlock, PageKind, Print, PrintDoc, Run};

    fn run(text: &str) -> Run {
        Run {
            text: text.into(),
            bold: false,
            italic: false,
            code: false,
            link: None,
        }
    }

    fn para(anchor: Option<&str>, text: &str) -> PBlock {
        PBlock::Para {
            anchor: anchor.map(str::to_string),
            runs: vec![run(text)],
        }
    }

    fn heading(level: u8, anchor: Option<&str>, text: &str) -> PBlock {
        PBlock::Heading {
            level,
            anchor: anchor.map(str::to_string),
            runs: vec![run(text)],
        }
    }

    fn doc(kind: PageKind, page: &str, title: &str, blocks: Vec<PBlock>) -> PrintDoc {
        let slashes = page.matches('/').count();
        PrintDoc {
            kind,
            id: String::new(),
            title: title.into(),
            page: page.into(),
            depth: slashes.saturating_sub(usize::from(kind == PageKind::Folder)),
            blocks,
        }
    }

    fn print(docs: Vec<PrintDoc>) -> Print {
        Print {
            title: "Wallets".into(),
            description: Some("How the wallet works.".into()),
            label: "v1.2".into(),
            scope: "canon".into(),
            docs,
            blobs: BTreeMap::new(),
            warnings: vec![],
            // a hand-built print has no project; the layout does not read it
            rdy_dir: std::path::PathBuf::new(),
        }
    }

    fn a4() -> PageSpec {
        PageSpec {
            width: 595.28,
            height: 841.89,
            margin: 56.69,
        }
    }

    fn no_size(_: &Hash) -> Option<(u32, u32)> {
        None
    }

    /// The text items of a page above the footer: (x, y, face, size, text).
    fn texts(page: &LPage) -> Vec<(f32, f32, Face, f32, String)> {
        page.items
            .iter()
            .filter_map(|i| match i {
                LItem::Text {
                    x,
                    y,
                    face,
                    size,
                    text,
                } if *y > 40.0 => Some((*x, *y, *face, *size, text.clone())),
                _ => None,
            })
            .collect()
    }

    fn width(fonts: &Fonts, face: Face, size: f32, s: &str) -> f32 {
        s.chars().map(|c| fonts.advance(face, c, size)).sum()
    }

    #[test]
    fn ec8_a_paragraph_wraps_at_the_text_width() {
        let fonts = Fonts::load();
        let spec = a4();
        let words: Vec<String> = (0..120)
            .map(|i| ["lorem", "ipsum", "dolor", "sit", "amet,"][i % 5].to_string())
            .collect();
        let text = words.join(" ");
        let p = print(vec![doc(
            PageKind::Doc,
            "w.html",
            "Wrap",
            vec![para(None, &text)],
        )]);
        let laid = lay_out(&p, &fonts, &spec, &no_size);
        let lines: Vec<_> = texts(&laid.pages[1])
            .into_iter()
            .filter(|t| t.2 == Face::Sans && t.3 == 10.5)
            .collect();
        assert!(lines.len() >= 3, "{lines:?}");
        let right = spec.width - spec.margin;
        for (i, (x, y, face, size, s)) in lines.iter().enumerate() {
            assert!((*x - spec.margin).abs() < 1e-3, "line {i} starts at {x}");
            assert!(
                x + width(&fonts, *face, *size, s) <= right + 1e-3,
                "line {i}"
            );
            assert!(!s.starts_with(' ') && !s.ends_with(' '), "{s:?}");
            if let Some(next) = lines.get(i + 1) {
                // body lines are 14 pt apart
                assert!((y - next.1 - 14.0).abs() < 1e-3, "{y} → {}", next.1);
                // greedy: the next line's first word did not fit on this one
                let first = next.4.split(' ').next().unwrap();
                let with = format!("{s} {first}");
                assert!(x + width(&fonts, *face, *size, &with) > right, "{with:?}");
            }
        }
        let joined: Vec<_> = lines.iter().map(|l| l.4.as_str()).collect();
        assert_eq!(joined.join(" "), text);
    }

    #[test]
    fn ec8_a_long_word_breaks_between_characters() {
        let fonts = Fonts::load();
        let spec = a4();
        let word = "x".repeat(400);
        let p = print(vec![doc(
            PageKind::Doc,
            "w.html",
            "Long",
            vec![para(None, &format!("{word} tail"))],
        )]);
        let laid = lay_out(&p, &fonts, &spec, &no_size);
        let lines: Vec<_> = texts(&laid.pages[1])
            .into_iter()
            .filter(|t| t.2 == Face::Sans && t.3 == 10.5)
            .collect();
        assert!(lines.len() >= 3, "{lines:?}");
        let right = spec.width - spec.margin;
        let x_adv = fonts.advance(Face::Sans, 'x', 10.5);
        let mut got = String::new();
        for (x, _, face, size, s) in &lines {
            let w = width(&fonts, *face, *size, s);
            assert!(x + w <= right + 1e-3);
            if s.chars().all(|c| c == 'x') && got.len() + s.len() < word.len() {
                // a full line: one more character would not fit
                assert!(x + w + x_adv > right, "{s:?}");
            }
            got.push_str(s);
        }
        assert_eq!(got, format!("{word} tail"));
    }

    #[test]
    fn ec8_each_doc_starts_on_a_new_page() {
        let fonts = Fonts::load();
        let spec = a4();
        let p = print(vec![
            doc(PageKind::Folder, "a/index.html", "A", vec![para(None, "a")]),
            doc(PageKind::Doc, "a/x.html", "X", vec![para(None, "x")]),
            doc(
                PageKind::Folder,
                "a/b/index.html",
                "B",
                vec![para(None, "b")],
            ),
            doc(PageKind::Doc, "a/b/y.html", "Y", vec![para(None, "y")]),
            doc(PageKind::Folder, "c/index.html", "C", vec![]),
            doc(PageKind::Doc, "z.html", "Z", vec![para(None, "z")]),
        ]);
        let laid = lay_out(&p, &fonts, &spec, &no_size);
        let page_of = |pg: &str| laid.anchors[&(pg.to_string(), None)].0;
        // the title part is page 0; a top-level folder and every doc start a page; a nested
        // folder follows on
        assert_eq!(page_of("a/index.html"), 1);
        assert_eq!(page_of("a/x.html"), 2);
        assert_eq!(page_of("a/b/index.html"), 2);
        assert_eq!(page_of("a/b/y.html"), 3);
        assert_eq!(page_of("c/index.html"), 4);
        assert_eq!(page_of("z.html"), 5);
        assert_eq!(laid.pages.len(), 6);
        // the title part holds the name, the description, the label and the scope
        let title: Vec<String> = texts(&laid.pages[0]).into_iter().map(|t| t.4).collect();
        for want in ["Wallets", "How the wallet works.", "v1.2", "canon"] {
            assert!(
                title.iter().any(|t| t.contains(want)),
                "{want} in {title:?}"
            );
        }
        // a page starts at the top of the text area, and each page has its number
        let top = spec.height - spec.margin;
        assert_eq!(laid.anchors[&("z.html".to_string(), None)].1, top);
        for (i, page) in laid.pages.iter().enumerate() {
            assert_eq!(page.number, i + 1);
            let n = (i + 1).to_string();
            let footer = page.items.iter().find_map(|it| match it {
                LItem::Text { x, y, text, .. } if *y < 40.0 => Some((*x, *y, text.clone())),
                _ => None,
            });
            let (x, y, text) = footer.expect("a page number");
            assert_eq!(text, n);
            assert!((y - 28.3465).abs() < 0.01, "10 mm from the bottom: {y}");
            let w = width(&fonts, Face::Sans, PAGE_NUMBER_SIZE, &n);
            assert!((x + w / 2.0 - spec.width / 2.0).abs() < 1e-3, "centred");
        }
    }

    #[test]
    fn ec8_a_table_row_never_splits_across_pages() {
        let fonts = Fonts::load();
        let spec = a4();
        let cell = |r: usize, c: usize| vec![run(&vec![format!("r{r:02}c{c}"); 14].join(" "))];
        let rows: Vec<Vec<Vec<Run>>> = (0..40)
            .map(|r| (0..3).map(|c| cell(r, c)).collect())
            .collect();
        let mut tall = vec![cell(99, 0), cell(99, 1)];
        tall.push(vec![run(&vec!["tall"; 3000].join(" "))]);
        let mut all_rows = rows.clone();
        all_rows.push(tall);
        let p = print(vec![doc(
            PageKind::Doc,
            "t.html",
            "Table",
            vec![
                para(None, "before"),
                PBlock::Table {
                    anchor: None,
                    head: vec![vec![run("H1")], vec![run("H2")], vec![run("H3")]],
                    rows: all_rows,
                },
            ],
        )]);
        let laid = lay_out(&p, &fonts, &spec, &no_size);
        let mut pages_used = BTreeSet::new();
        for r in 0..40 {
            let tag = format!("r{r:02}c");
            let pages: BTreeSet<usize> = laid
                .pages
                .iter()
                .enumerate()
                .filter(|(_, pg)| texts(pg).iter().any(|t| t.4.contains(&tag)))
                .map(|(i, _)| i)
                .collect();
            assert_eq!(pages.len(), 1, "row {r} is on pages {pages:?}");
            pages_used.extend(pages);
        }
        assert!(pages_used.len() > 1, "the rows fill more than one page");
        // the head row is Bold
        let head = texts(&laid.pages[1]);
        assert!(head.iter().any(|t| t.4 == "H1" && t.2 == Face::Bold));
        // a row taller than a page splits
        let tall_pages = laid
            .pages
            .iter()
            .filter(|pg| texts(pg).iter().any(|t| t.4.contains("tall")))
            .count();
        assert!(tall_pages > 1);
        // every cell line stays inside its column
        let col = (spec.width - 2.0 * spec.margin) / 3.0;
        for pg in &laid.pages {
            for (x, _, face, size, s) in texts(pg) {
                if s.contains("r0") {
                    let left = spec.margin + col * ((x - spec.margin) / col).floor();
                    assert!(
                        x + width(&fonts, face, size, &s) <= left + col + 1e-3,
                        "{s}"
                    );
                }
            }
        }
    }

    #[test]
    fn ec8_a_char_with_no_glyph_prints_as_a_question_mark_and_is_reported() {
        let fonts = Fonts::load();
        let p = print(vec![doc(
            PageKind::Doc,
            "g.html",
            "Glyphs 日",
            vec![
                para(None, "a 中 b → c"),
                PBlock::Code {
                    anchor: None,
                    lang: None,
                    text: "x → y ≤ z".into(),
                },
            ],
        )]);
        let laid = lay_out(&p, &fonts, &a4(), &no_size);
        let all: Vec<String> = texts(&laid.pages[1]).into_iter().map(|t| t.4).collect();
        assert!(all.contains(&"Glyphs ?".to_string()), "{all:?}");
        assert!(all.contains(&"a ? b → c".to_string()), "{all:?}");
        assert!(all.contains(&"x ? y ? z".to_string()), "{all:?}");
        assert_eq!(
            laid.missing,
            BTreeSet::from(['中', '日', '→', '≤']),
            "Sans has → and ≤; Courier has neither"
        );
        let code = texts(&laid.pages[1])
            .into_iter()
            .find(|t| t.4.starts_with("x "))
            .unwrap();
        assert_eq!((code.2, code.3), (Face::Mono, 9.0));
    }

    #[test]
    fn ec8_a_script_that_needs_shaping_prints_as_question_marks_on_every_path() {
        // RULING F1 (final review I1): DejaVu holds Arabic and Hebrew letters, but the writer
        // neither joins them nor sets them right to left, so each prints as `?` and joins
        // `missing`, in the title part, titles, headings, styled runs, tables, lists, captions,
        // figure lines and callout labels alike
        let fonts = Fonts::load();
        let fig = Hash::of(b"fig");
        let size = move |h: &Hash| (*h == fig).then_some((400, 300));
        let mut p = print(vec![doc(
            PageKind::Doc,
            "a.html",
            "باب",
            vec![
                heading(2, None, "Head ع"),
                PBlock::Para {
                    anchor: None,
                    runs: vec![
                        run("plain ح "),
                        Run {
                            bold: true,
                            ..run("bold ش")
                        },
                        run(" "),
                        Run {
                            italic: true,
                            ..run("it ب")
                        },
                    ],
                },
                PBlock::Table {
                    anchor: None,
                    head: vec![vec![run("head ج")]],
                    rows: vec![vec![vec![run("cell ث")]]],
                },
                PBlock::List {
                    ordered: false,
                    start: 1,
                    items: vec![vec![para(None, "item ש")]],
                },
                PBlock::Figure {
                    anchor: None,
                    blob: Some(fig),
                    alt: "shown".into(),
                    caption: vec![run("cap ة")],
                },
                PBlock::Figure {
                    anchor: None,
                    blob: None,
                    alt: "alt ت".into(),
                    caption: vec![],
                },
                PBlock::Callout {
                    anchor: None,
                    label: "ملاحظة".into(),
                    blocks: vec![para(None, "inside ق")],
                },
            ],
        )]);
        p.title = "دليل".into();
        let laid = lay_out(&p, &fonts, &a4(), &size);
        let items: Vec<&LItem> = laid.pages.iter().flat_map(|pg| &pg.items).collect();
        let has = |want: &str, face: Face| {
            items.iter().any(
                |i| matches!(i, LItem::Text { text, face: f, .. } if text == want && *f == face),
            )
        };
        let shaped = |c: char| matches!(u32::from(c), 0x0590..=0x08FF);
        for i in &items {
            if let LItem::Text { text, .. } = i {
                assert!(!text.chars().any(shaped), "{text:?} prints a shaped letter");
            }
        }
        assert!(has("????", Face::Bold), "the title part");
        assert!(has("???", Face::Bold), "the doc's title");
        assert!(has("Head ?", Face::Bold), "a heading");
        assert!(has("plain ?", Face::Sans), "a plain run");
        assert!(has("bold ?", Face::Bold), "a bold run");
        assert!(has("it ?", Face::Italic), "an italic run");
        assert!(has("head ?", Face::Bold), "a table's head");
        assert!(has("cell ?", Face::Sans), "a table cell");
        assert!(has("item ?", Face::Sans), "a list item");
        assert!(has("cap ?", Face::Italic), "a caption");
        assert!(has("Figure: alt ?", Face::Italic), "a figure line");
        assert!(has("??????", Face::Bold), "a callout label");
        assert!(has("inside ?", Face::Sans), "a callout's text");
        let want: BTreeSet<char> = "دليلبابعحشبجثשةتملاحظةق".chars().collect();
        assert_eq!(laid.missing, want);
    }

    #[test]
    fn ec8_anchors_record_their_page_and_place() {
        let fonts = Fonts::load();
        let spec = a4();
        let mut blocks = vec![heading(2, Some("intro"), "Intro")];
        for i in 0..90 {
            blocks.push(para(Some(&format!("p{i}")), &format!("para {i} words")));
        }
        let p = print(vec![
            doc(PageKind::Doc, "a.html", "A", vec![para(None, "first")]),
            doc(PageKind::Doc, "d.html", "D", blocks),
        ]);
        let laid = lay_out(&p, &fonts, &spec, &no_size);
        let at = |anchor: &str| laid.anchors[&("d.html".to_string(), Some(anchor.to_string()))];
        let top = spec.height - spec.margin;
        assert_eq!(laid.anchors[&("a.html".to_string(), None)], (1, top));
        assert_eq!(laid.anchors[&("d.html".to_string(), None)], (2, top));
        let mut pages = BTreeSet::new();
        for i in 0..90 {
            let (pg, y) = at(&format!("p{i}"));
            pages.insert(pg);
            let want = format!("para {i} words");
            let line = texts(&laid.pages[pg])
                .into_iter()
                .find(|t| t.4 == want)
                .unwrap_or_else(|| panic!("{want} on page {pg}"));
            // the anchor is the top of its block: the first line's baseline sits under it
            assert!(line.1 < y && line.1 > y - 14.0, "{want}: {} vs {y}", line.1);
        }
        assert!(pages.len() >= 2, "{pages:?}");
        let (pg, y) = at("intro");
        assert_eq!(pg, 2);
        let line = texts(&laid.pages[2])
            .into_iter()
            .find(|t| t.4 == "Intro")
            .unwrap();
        assert!(line.1 < y && line.2 == Face::Bold && line.3 == 16.0);
        // the anchors are only the print's: no page or anchor that is not in it
        assert_eq!(laid.anchors.len(), 2 + 1 + 90);
    }

    #[test]
    fn ec8_the_outline_follows_the_docs_and_headings() {
        let fonts = Fonts::load();
        let spec = a4();
        let p = print(vec![
            doc(
                PageKind::Doc,
                "a.html",
                "Alpha",
                vec![
                    heading(2, Some("one"), "One"),
                    para(None, "text"),
                    PBlock::Heading {
                        level: 3,
                        anchor: None,
                        runs: vec![
                            run("Two "),
                            Run {
                                bold: true,
                                ..run("parts")
                            },
                        ],
                    },
                ],
            ),
            doc(PageKind::Folder, "b/index.html", "Beta", vec![]),
            doc(
                PageKind::Doc,
                "b/c.html",
                "Gamma",
                vec![heading(1, None, "Three"), heading(5, None, "Five")],
            ),
        ]);
        let laid = lay_out(&p, &fonts, &spec, &no_size);
        let got: Vec<(&str, u8, usize)> = laid
            .outline
            .iter()
            .map(|o| (o.title.as_str(), o.level, o.page))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Alpha", 0, 1),
                ("One", 2, 1),
                ("Two parts", 3, 1),
                ("Beta", 0, 2),
                ("Gamma", 0, 3),
                ("Three", 1, 3),
                ("Five", 5, 3),
            ]
        );
        let top = spec.height - spec.margin;
        assert_eq!(laid.outline[0].y, top);
        let one = laid.anchors[&("a.html".to_string(), Some("one".to_string()))];
        assert_eq!((laid.outline[1].page, laid.outline[1].y), one);
        assert!(laid.outline[2].y < laid.outline[1].y);
        // the sizes of the heading levels
        let sizes: BTreeMap<String, f32> = texts(&laid.pages[3])
            .into_iter()
            .map(|t| (t.4, t.3))
            .collect();
        assert_eq!(sizes["Three"], 20.0);
        assert_eq!(sizes["Five"], 11.5);
    }

    #[test]
    fn ec8_layout_is_deterministic() {
        let fonts = Fonts::load();
        let spec = a4();
        let wide = Hash::of(b"wide");
        let tall = Hash::of(b"tall");
        let odd = Hash::of(b"odd");
        let size = move |h: &Hash| {
            if *h == wide {
                Some((400, 300))
            } else if *h == tall {
                Some((100, 2000))
            } else {
                None
            }
        };
        let link = Run {
            link: Some(Href::Page {
                page: "b.html".into(),
                anchor: Some("x".into()),
            }),
            ..run("a link")
        };
        let ext = Run {
            link: Some(Href::External("https://example.org/".into())),
            ..run("out")
        };
        let mut p = print(vec![
            doc(
                PageKind::Doc,
                "a.html",
                "Every block",
                vec![
                    heading(2, Some("h"), "Head"),
                    PBlock::Para {
                        anchor: None,
                        runs: vec![
                            run("plain "),
                            Run {
                                bold: true,
                                ..run("bold ")
                            },
                            Run {
                                italic: true,
                                ..run("italic ")
                            },
                            Run {
                                code: true,
                                ..run("code")
                            },
                            run(" then "),
                            link,
                            run(" and "),
                            ext,
                            run(".\nA second line."),
                        ],
                    },
                    PBlock::List {
                        ordered: false,
                        start: 1,
                        items: vec![
                            vec![para(None, "one")],
                            vec![
                                para(None, "two"),
                                PBlock::List {
                                    ordered: true,
                                    start: 3,
                                    items: vec![vec![para(None, "three")], vec![]],
                                },
                            ],
                        ],
                    },
                    PBlock::Code {
                        anchor: Some("c".into()),
                        lang: Some("rust".into()),
                        text: format!("fn main() {{\n\tlet x = 1;\n}}\n{}", "y".repeat(200)),
                    },
                    PBlock::Figure {
                        anchor: Some("f1".into()),
                        blob: Some(wide),
                        alt: "wide".into(),
                        caption: vec![run("A wide figure")],
                    },
                    PBlock::Figure {
                        anchor: None,
                        blob: Some(tall),
                        alt: "tall".into(),
                        caption: vec![],
                    },
                    PBlock::Figure {
                        anchor: None,
                        blob: Some(odd),
                        alt: "odd one".into(),
                        caption: vec![],
                    },
                    PBlock::Callout {
                        anchor: Some("n".into()),
                        label: "Note".into(),
                        blocks: vec![para(None, "inside the callout")],
                    },
                ],
            ),
            doc(
                PageKind::Doc,
                "b.html",
                "B",
                vec![para(Some("x"), "target")],
            ),
            doc(PageKind::Glossary, "glossary.html", "Glossary", vec![]),
        ]);
        p.blobs.insert(
            wide,
            Blob {
                media_type: "image/png".into(),
                bytes: vec![],
                file: String::new(),
            },
        );
        let one = lay_out(&p, &fonts, &spec, &size);
        let two = lay_out(&p, &fonts, &spec, &size);
        assert_eq!(one, two);

        let items: Vec<&LItem> = one.pages.iter().flat_map(|pg| &pg.items).collect();
        let text_w = spec.width - 2.0 * spec.margin;
        let images: Vec<(f32, f32)> = items
            .iter()
            .filter_map(|i| match i {
                LItem::Image { w, h, .. } => Some((*w, *h)),
                _ => None,
            })
            .collect();
        assert_eq!(images.len(), 2);
        // scaled to the text width; at most 60% of the page height
        assert!((images[0].0 - text_w).abs() < 1e-3);
        assert!((images[0].1 - text_w * 0.75).abs() < 1e-2);
        assert!((images[1].1 - 0.6 * spec.height).abs() < 1e-2);
        assert!((images[1].0 - 0.6 * spec.height / 20.0).abs() < 1e-2);
        let has = |want: &str, face: Face| {
            items.iter().any(
                |i| matches!(i, LItem::Text { text, face: f, .. } if text == want && *f == face),
            )
        };
        assert!(has("Figure: odd one", Face::Italic));
        assert!(has("A wide figure", Face::Italic));
        assert!(has("•", Face::Sans));
        assert!(has("3.", Face::Sans));
        assert!(has("4.", Face::Sans));
        assert!(has("Note", Face::Bold));
        assert!(has("bold", Face::Bold));
        assert!(has("italic", Face::Italic) || has("italic ", Face::Italic));
        assert!(has("code", Face::Mono));
        assert!(has("    let x = 1;", Face::Mono), "a tab prints as spaces");
        assert!(
            items
                .iter()
                .any(|i| matches!(i, LItem::Rule { width, .. } if *width == 2.0))
        );
        let links: Vec<&Href> = items
            .iter()
            .filter_map(|i| match i {
                LItem::Link { href, rect } => {
                    assert!(rect[0] < rect[2] && rect[1] < rect[3]);
                    Some(href)
                }
                _ => None,
            })
            .collect();
        assert_eq!(links.len(), 2);
        assert!(matches!(links[1], Href::External(u) if u == "https://example.org/"));
        assert!(
            one.anchors
                .contains_key(&("glossary.html".to_string(), None))
        );
        assert!(one.missing.is_empty(), "{:?}", one.missing);
    }

    /// The ink of `text` set at `x` in `face` and `size`: (left, right), from the glyphs'
    /// outlines, not their advances.
    fn ink(fonts: &Fonts, face: Face, size: f32, x: f32, text: &str) -> (f32, f32) {
        use crate::print::fonts::{SANS, SANS_BOLD};
        let bytes = match face {
            Face::Sans | Face::Italic => SANS,
            Face::Bold | Face::BoldItalic => SANS_BOLD,
            Face::Mono => panic!("ink is measured in the DejaVu faces only"),
        };
        let f = ttf_parser::Face::parse(bytes, 0).unwrap();
        let scale = size / f32::from(f.units_per_em());
        let (mut left, mut right) = (f32::MAX, f32::MIN);
        let mut pen = x;
        for c in text.chars() {
            let g = ttf_parser::GlyphId(fonts.glyph(face, c).unwrap());
            if let Some(b) = f.glyph_bounding_box(g) {
                left = left.min(pen + f32::from(b.x_min) * scale);
                right = right.max(pen + f32::from(b.x_max) * scale);
            }
            pen += fonts.advance(face, c, size);
        }
        (left, right)
    }

    /// Asserts that no two text items on one baseline of `laid` share ink.
    fn assert_no_ink_overlaps(fonts: &Fonts, laid: &Laid) {
        for (pi, pg) in laid.pages.iter().enumerate() {
            let t = texts(pg);
            for (i, a) in t.iter().enumerate() {
                for b in &t[i + 1..] {
                    if (a.1 - b.1).abs() > 1e-3 {
                        continue;
                    }
                    let ia = ink(fonts, a.2, a.3, a.0, &a.4);
                    let ib = ink(fonts, b.2, b.3, b.0, &b.4);
                    assert!(
                        ia.1 < ib.0 || ib.1 < ia.0,
                        "page {pi}: {:?} at {} and {:?} at {} share ink: {ia:?} {ib:?}",
                        a.4,
                        a.0,
                        b.4,
                        b.0
                    );
                }
            }
        }
    }

    #[test]
    fn ec8_a_list_marker_of_two_or_more_digits_stays_clear_of_its_item() {
        let fonts = Fonts::load();
        let spec = a4();
        let ol = |start: u32, n: usize| PBlock::List {
            ordered: true,
            start,
            items: (0..n)
                .map(|i| vec![para(None, &format!("item {start}+{i}"))])
                .collect(),
        };
        let mut blocks =
            crate::print::ir::from_html(r#"<ol start="10"><li>item</li></ol>"#, "doc.html");
        blocks.push(ol(1, 12));
        blocks.push(ol(99, 3));
        blocks.push(ol(123_456, 2));
        blocks.push(ol(u32::MAX, 2));
        // a nested list as the first block of an item: two markers on one line
        blocks.push(PBlock::List {
            ordered: true,
            start: 40,
            items: vec![vec![ol(70, 2)], vec![ol(100_000, 1)]],
        });
        let p = print(vec![doc(PageKind::Doc, "doc.html", "Lists", blocks)]);
        let laid = lay_out(&p, &fonts, &spec, &no_size);
        assert_no_ink_overlaps(&fonts, &laid);
        let all: Vec<_> = laid.pages.iter().flat_map(texts).collect();
        let x0 = spec.margin;
        let item_x = |text: &str| {
            all.iter()
                .find(|t| t.4 == text)
                .unwrap_or_else(|| panic!("{text} in {all:?}"))
                .0
        };
        // the indent is 14 pt a level, whatever the markers' widths
        assert_eq!(item_x("item"), x0 + 14.0);
        assert_eq!(item_x("item 1+11"), x0 + 14.0);
        assert_eq!(item_x("item 4294967295+1"), x0 + 14.0);
        assert_eq!(item_x("item 70+1"), x0 + 28.0);
        assert_eq!(item_x("item 100000+0"), x0 + 28.0);
        // each marker is whole, and inside its list's indent
        for (n, level) in [
            ("10.", 0),
            ("9.", 0),
            ("12.", 0),
            ("101.", 0),
            ("123457.", 0),
            ("4294967296.", 0),
            ("40.", 0),
            ("41.", 0),
            ("70.", 1),
            ("71.", 1),
            ("100000.", 1),
        ] {
            let m = all
                .iter()
                .find(|t| t.4 == n && t.2 == Face::Sans)
                .unwrap_or_else(|| panic!("marker {n} in {all:?}"));
            let left = x0 + 14.0 * level as f32;
            let (l, r) = ink(&fonts, m.2, m.3, m.0, &m.4);
            assert!(
                m.0 >= left - 1e-3 && l >= left - 1e-3,
                "{n} starts at {}",
                m.0
            );
            assert!(r < left + 14.0, "{n} ends at {r}");
        }
    }

    #[test]
    fn ec8_an_oversized_first_table_row_anchors_on_the_page_of_its_first_line() {
        let fonts = Fonts::load();
        let spec = a4();
        let mut blocks: Vec<PBlock> = (0..35).map(|i| para(None, &format!("p{i}"))).collect();
        let cell: Vec<String> = (0..60).map(|i| format!("H{i:03}")).collect();
        blocks.push(PBlock::Table {
            anchor: Some("t".into()),
            head: vec![vec![run(&cell.join("\n"))]],
            rows: vec![],
        });
        let p = print(vec![doc(PageKind::Doc, "doc.html", "Tables", blocks)]);
        let laid = lay_out(&p, &fonts, &spec, &no_size);
        let (pg, y) = laid.anchors[&("doc.html".to_string(), Some("t".to_string()))];
        let (first_pg, first) = laid
            .pages
            .iter()
            .enumerate()
            .find_map(|(i, page)| {
                texts(page)
                    .into_iter()
                    .find(|t| t.4 == "H000")
                    .map(|t| (i, t))
            })
            .expect("H000 is printed");
        assert!(
            y >= spec.margin,
            "the anchor {y} is above the bottom margin"
        );
        assert_eq!(pg, first_pg, "the anchor is on the page of the first line");
        // the anchor is the top of the table: the first line's baseline sits just under it
        assert!(
            first.1 < y && first.1 > y - 14.0 - 3.0 - 1e-3,
            "{} vs {y}",
            first.1
        );
        // the table's first rule is on that page too: none is left behind
        let rules_before: usize = laid.pages[..first_pg]
            .iter()
            .map(|page| {
                page.items
                    .iter()
                    .filter(|i| matches!(i, LItem::Rule { width, .. } if *width == 0.5))
                    .count()
            })
            .sum();
        assert_eq!(rules_before, 0);
    }
}
