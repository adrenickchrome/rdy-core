//! The PDF's fonts (RULING F1): DejaVu Sans and DejaVu Sans Bold from the brand kit, compiled in,
//! and Courier, the PDF standard font, for code. `skrifa` reads the DejaVu tables (cmap, hmtx,
//! head, hhea and OS/2); Courier's metrics are its AFM's (every glyph 600 units wide).
//!
//! Italic is Sans slanted 12 degrees, and Bold Italic is Bold slanted the same way: the PDF writer
//! slants them with the text matrix, so they share their upright face's glyphs and advances.
//! Courier is written with WinAnsiEncoding, so its glyph for a character is the character's
//! Windows-1252 code, and a character outside Windows-1252 has none.

use skrifa::raw::tables::cmap::{CmapSubtable, PlatformId};
use skrifa::raw::{FontRef, ReadError, TableProvider};

/// DejaVu Sans, the brand kit's file (`05-Editable/DejaVuSans.ttf`).
pub const SANS: &[u8] = include_bytes!("../../fonts/DejaVuSans.ttf");
/// DejaVu Sans Bold, the brand kit's file (`05-Editable/DejaVuSans-Bold.ttf`).
pub const SANS_BOLD: &[u8] = include_bytes!("../../fonts/DejaVuSans-Bold.ttf");

/// How a piece of text looks in the PDF.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Face {
    Sans,
    Bold,
    /// Sans slanted 12 degrees.
    Italic,
    /// Bold slanted 12 degrees.
    BoldItalic,
    /// Courier, the standard font.
    Mono,
}

/// Courier's advance, ascender and descender, in units of 1/1000 of the size (its AFM).
const COURIER_ADVANCE: f32 = 600.0;
const COURIER_ASCENT: f32 = 629.0;
const COURIER_DESCENT: f32 = -157.0;

/// Windows-1252's row 0x80–0x9f; `None` where the code page has no character.
const CP1252_HIGH: [Option<char>; 32] = [
    Some('€'),
    None,
    Some('‚'),
    Some('ƒ'),
    Some('„'),
    Some('…'),
    Some('†'),
    Some('‡'),
    Some('ˆ'),
    Some('‰'),
    Some('Š'),
    Some('‹'),
    Some('Œ'),
    None,
    Some('Ž'),
    None,
    None,
    Some('‘'),
    Some('’'),
    Some('“'),
    Some('”'),
    Some('•'),
    Some('–'),
    Some('—'),
    Some('˜'),
    Some('™'),
    Some('š'),
    Some('›'),
    Some('œ'),
    None,
    Some('ž'),
    Some('Ÿ'),
];

/// The Windows-1252 code of `c`, for the characters Courier prints: the printable ASCII range,
/// the code page's row 0x80–0x9f and Latin-1's 0xa0–0xff.
fn cp1252(c: char) -> Option<u8> {
    let n = u32::from(c);
    match n {
        0x20..=0x7e | 0xa0..=0xff => u8::try_from(n).ok(),
        _ => CP1252_HIGH
            .iter()
            .position(|h| *h == Some(c))
            .and_then(|i| u8::try_from(0x80 + i).ok()),
    }
}

/// The scripts that need shaping (joined letters, conjuncts, reordered or stacked marks) or
/// right-to-left layout, as code point ranges (closed). The writer sets each character on its own,
/// left to right, so a letter of these scripts would print in the wrong form and order, even
/// where DejaVu holds it (Hebrew, Arabic, NKo, Lao): RULING F1 prints them as `?`, with the glyph
/// warning. Scripts that DejaVu lacks (CJK, most of the rest) print as `?` without this list.
const SHAPED: [std::ops::RangeInclusive<u32>; 12] = [
    // Hebrew, Arabic, Syriac, Thaana, NKo, Samaritan, Mandaic, the Arabic supplements and
    // extensions: right to left, and Arabic, Syriac, NKo and Mandaic join their letters
    0x0590..=0x08ff,
    // the Indic scripts, Devanagari to Sinhala
    0x0900..=0x0dff,
    // Thai, up to its baht sign, which is a symbol and prints
    0x0e00..=0x0e3e,
    // the rest of Thai, Lao and Tibetan
    0x0e40..=0x0fff,
    // Myanmar
    0x1000..=0x109f,
    // Khmer and Mongolian
    0x1780..=0x18af,
    // Limbu, Tai Le, New Tai Lue, Khmer symbols, Buginese and Tai Tham
    0x1900..=0x1aaf,
    // Balinese, Sundanese, Batak and Lepcha
    0x1b00..=0x1c4f,
    // the Hebrew and Arabic presentation forms A
    0xfb1d..=0xfdff,
    // the Arabic presentation forms B (up to, not with, U+FEFF)
    0xfe70..=0xfefe,
    // the right-to-left scripts of the Supplementary Multilingual Plane
    0x10800..=0x10fff,
    // Mende Kikakui, Adlam and the Arabic mathematical letters
    0x1e800..=0x1efff,
];

/// Whether `c` belongs to a script that needs shaping or right-to-left layout ([`SHAPED`]).
fn needs_shaping(c: char) -> bool {
    let n = u32::from(c);
    SHAPED.iter().any(|r| r.contains(&n))
}

/// One compiled-in DejaVu face, read from its raw tables (`skrifa`'s `read-fonts`): its cmap
/// records in stored order, its unscaled hmtx advances, its head bounds and units per em, its
/// hhea ascent and descent, and the OS/2 cap height (the ascent when the table has none).
struct EmbeddedFont<'a> {
    font: FontRef<'a>,
    num_glyphs: u16,
    descriptor: FontDescriptor,
    ascent: i16,
    descent: i16,
}

/// What the PDF's font descriptor reads from a DejaVu face, in font units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FontDescriptor {
    pub(super) units_per_em: u16,
    /// The head table's `[x_min, y_min, x_max, y_max]`.
    pub(super) bounds: [i16; 4],
    /// The OS/2 cap height, or the ascent when the face has none.
    pub(super) cap_height: i16,
}

impl<'a> EmbeddedFont<'a> {
    fn parse(data: &'a [u8]) -> Result<EmbeddedFont<'a>, ReadError> {
        let font = FontRef::new(data)?;
        let head = font.head()?;
        let hhea = font.hhea()?;
        let ascent = hhea.ascender().to_i16();
        let cap_height = font
            .os2()
            .ok()
            .and_then(|o| o.s_cap_height())
            .unwrap_or(ascent);
        // the tables each lookup reads parse here first, so a lookup cannot fail on them
        font.cmap()?;
        font.hmtx()?;
        Ok(EmbeddedFont {
            num_glyphs: font.maxp()?.num_glyphs(),
            descriptor: FontDescriptor {
                units_per_em: head.units_per_em(),
                bounds: [head.x_min(), head.y_min(), head.x_max(), head.y_max()],
                cap_height,
            },
            ascent,
            descent: hhea.descender().to_i16(),
            font,
        })
    }

    /// The glyph for `c`: the first Unicode cmap subtable, in stored order, that gives an id
    /// gives it, and id 0 is no glyph. A Unicode subtable is a Unicode platform record, a Windows
    /// BMP record (encoding 1), or a Windows full-repertoire record (encoding 10) of format 12 or
    /// 13. The loop is bounded by the record count (a u16).
    fn glyph(&self, c: char) -> Option<u16> {
        let cmap = self.font.cmap().ok()?;
        let data = cmap.offset_data();
        for record in cmap.encoding_records() {
            let Ok(subtable) = record.subtable(data) else {
                continue;
            };
            let unicode = match record.platform_id() {
                PlatformId::Unicode => true,
                PlatformId::Windows => {
                    record.encoding_id() == 1
                        || (record.encoding_id() == 10
                            && matches!(
                                subtable,
                                CmapSubtable::Format12(_) | CmapSubtable::Format13(_)
                            ))
                }
                _ => false,
            };
            if !unicode {
                continue;
            }
            if let Some(g) = subtable
                .map_codepoint(c)
                .and_then(|g| u16::try_from(g.to_u32()).ok())
            {
                return Some(g).filter(|g| *g != 0);
            }
        }
        None
    }

    /// The unscaled advance of glyph `gid`; a glyph past the last metric takes the last one's
    /// advance, and an id past the font's glyphs has none.
    fn advance(&self, gid: u16) -> Option<u16> {
        if gid >= self.num_glyphs {
            return None;
        }
        let hmtx = self.font.hmtx().ok()?;
        let metrics = hmtx.h_metrics();
        metrics
            .get(usize::from(gid))
            .or(metrics.last())
            .map(|m| m.advance())
    }

    /// Tests only: the ink box of glyph `gid` from its unhinted outline in font units: every move,
    /// line and curve-control point, each bound truncated toward zero; `None` when it is empty.
    #[cfg(test)]
    fn ink_bounds(&self, gid: u16) -> Option<[i16; 4]> {
        use skrifa::MetadataProvider;
        use skrifa::instance::{LocationRef, Size};
        use skrifa::outline::DrawSettings;
        let glyph = self.font.outline_glyphs().get(skrifa::GlyphId::from(gid))?;
        let mut sink = BoundsSink::default();
        let drawn = glyph
            .draw(
                DrawSettings::unhinted(Size::unscaled(), LocationRef::default()),
                &mut sink,
            )
            .ok()?;
        // skrifa follows FreeType and moves a glyph sideways by its first phantom point (its
        // glyf x_min less its hmtx side bearing), which it reports as the adjusted side bearing;
        // the ink is measured where the glyf table places the points, so the move is undone
        // (O5 ruling, task-1, 2026-10-23)
        sink.bounds(drawn.lsb.unwrap_or(0.0))
    }
}

/// Tests only: the box of every point an outline passes to its pen.
#[cfg(test)]
#[derive(Default)]
struct BoundsSink {
    /// `[x_min, y_min, x_max, y_max]`, None until the first point.
    rect: Option<[f32; 4]>,
}

#[cfg(test)]
impl BoundsSink {
    fn point(&mut self, x: f32, y: f32) {
        let r = self.rect.get_or_insert([x, y, x, y]);
        *r = [r[0].min(x), r[1].min(y), r[2].max(x), r[3].max(y)];
    }

    /// The box moved `dx` along x, with each bound truncated toward zero; `None` when it is
    /// empty or a bound is outside i16.
    fn bounds(&self, dx: f32) -> Option<[i16; 4]> {
        let trunc = |v: f32| {
            let t = v.trunc();
            (t >= f32::from(i16::MIN) && t <= f32::from(i16::MAX)).then_some(t as i16)
        };
        let [a, b, c, d] = self.rect?;
        Some([trunc(a + dx)?, trunc(b)?, trunc(c + dx)?, trunc(d)?])
    }
}

#[cfg(test)]
impl skrifa::outline::OutlinePen for BoundsSink {
    fn move_to(&mut self, x: f32, y: f32) {
        self.point(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.point(x, y);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.point(cx0, cy0);
        self.point(x, y);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.point(cx0, cy0);
        self.point(cx1, cy1);
        self.point(x, y);
    }
    fn close(&mut self) {}
}

/// The two parsed DejaVu faces.
pub struct Fonts {
    sans: EmbeddedFont<'static>,
    bold: EmbeddedFont<'static>,
}

impl Fonts {
    /// Parses the two compiled-in faces.
    pub fn load() -> Fonts {
        let parse = |bytes: &'static [u8]| {
            EmbeddedFont::parse(bytes).expect("the compiled-in DejaVu font parses")
        };
        Fonts {
            sans: parse(SANS),
            bold: parse(SANS_BOLD),
        }
    }

    /// The DejaVu face `face` draws with; `None` for Courier.
    fn embedded(&self, face: Face) -> Option<&EmbeddedFont<'static>> {
        match face {
            Face::Sans | Face::Italic => Some(&self.sans),
            Face::Bold | Face::BoldItalic => Some(&self.bold),
            Face::Mono => None,
        }
    }

    /// The glyph for `c` in `face`; None when the face has none (Mono: outside Windows-1252),
    /// and for a character of a script that needs shaping or right-to-left layout
    /// ([`needs_shaping`]), which the writer does not do (RULING F1): it prints as `?`.
    pub fn glyph(&self, face: Face, c: char) -> Option<u16> {
        if needs_shaping(c) {
            return None;
        }
        match self.embedded(face) {
            Some(f) => f.glyph(c),
            None => cp1252(c).map(u16::from),
        }
    }

    /// The advance of `c` at `size` points (`?`'s advance when there is no glyph).
    pub fn advance(&self, face: Face, c: char, size: f32) -> f32 {
        let Some(f) = self.embedded(face) else {
            return COURIER_ADVANCE * size / 1000.0;
        };
        let g = self
            .glyph(face, c)
            .or_else(|| self.glyph(face, '?'))
            .unwrap_or(0);
        let units = f.advance(g).unwrap_or(0);
        f32::from(units) * size / f32::from(f.descriptor.units_per_em)
    }

    /// How far the face rises above the baseline at `size` points.
    pub fn ascent(&self, face: Face, size: f32) -> f32 {
        match self.embedded(face) {
            Some(f) => f32::from(f.ascent) * size / f32::from(f.descriptor.units_per_em),
            None => COURIER_ASCENT * size / 1000.0,
        }
    }

    /// How far the face falls below the baseline at `size` points, as a negative number (the
    /// font's own sign, and the PDF's).
    pub fn descent(&self, face: Face, size: f32) -> f32 {
        match self.embedded(face) {
            Some(f) => f32::from(f.descent) * size / f32::from(f.descriptor.units_per_em),
            None => COURIER_DESCENT * size / 1000.0,
        }
    }

    /// What the PDF's font descriptor of `face` reads; `None` for Courier.
    pub(super) fn descriptor(&self, face: Face) -> Option<FontDescriptor> {
        self.embedded(face).map(|f| f.descriptor)
    }

    /// Tests only: the ink box of glyph `gid` in `face`, in font units, from its unhinted outline
    /// points (`[x_min, y_min, x_max, y_max]`); `None` for an empty outline and for Courier.
    #[cfg(test)]
    pub(super) fn ink_bounds(&self, face: Face, gid: u16) -> Option<[i16; 4]> {
        self.embedded(face)?.ink_bounds(gid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_model::Hash;

    /// Every Unicode scalar value, in ascending code point order (1112064 of them).
    fn scalars() -> impl Iterator<Item = char> {
        (0..=0x10_ffff_u32).filter_map(char::from_u32)
    }

    /// One kind of ordered comparison data: each item's encoding, back to back, in order.
    #[derive(Default)]
    struct Items(Vec<u8>);

    impl Items {
        /// Starts the next item and returns the buffer to encode it into.
        fn item(&mut self) -> &mut Vec<u8> {
            &mut self.0
        }
        fn digest(&self) -> String {
            Hash::of(&self.0).to_string()
        }
    }

    /// An option: byte 0, or byte 1 and then the value's little-endian bytes.
    fn opt<const N: usize>(out: &mut Vec<u8>, v: Option<[u8; N]>) {
        match v {
            None => out.push(0),
            Some(b) => {
                out.push(1);
                out.extend_from_slice(&b);
            }
        }
    }

    fn i16s(v: [i16; 4]) -> [u8; 8] {
        let mut b = [0; 8];
        for (i, x) in v.iter().enumerate() {
            b[2 * i..2 * i + 2].copy_from_slice(&x.to_le_bytes());
        }
        b
    }

    /// The reader's raw view of one DejaVu face: its cmap (0 is no glyph, before shaping), each
    /// glyph's unscaled advance and the face's metrics in font units.
    fn raw_glyph(fonts: &Fonts, face: Face, c: char) -> Option<u16> {
        fonts.embedded(face)?.glyph(c)
    }
    fn num_glyphs(fonts: &Fonts, face: Face) -> u16 {
        fonts.embedded(face).expect("a DejaVu face").num_glyphs
    }
    fn advance_units(fonts: &Fonts, face: Face, gid: u16) -> Option<u16> {
        fonts.embedded(face).expect("a DejaVu face").advance(gid)
    }
    /// (units per em, head bounds, ascent, descent, cap height or its ascent fallback)
    fn face_metrics(fonts: &Fonts, face: Face) -> (u16, [i16; 4], i16, i16, i16) {
        let f = fonts.embedded(face).expect("a DejaVu face");
        let d = f.descriptor;
        (d.units_per_em, d.bounds, f.ascent, f.descent, d.cap_height)
    }

    /// One DejaVu font's complete comparison data, by kind.
    struct FontItems {
        cmap: Items,
        advances: Items,
        ink: Items,
        metrics: Items,
    }

    impl FontItems {
        /// [cmap, advances, ink boxes, metrics]
        fn digests(&self) -> [String; 4] {
            [&self.cmap, &self.advances, &self.ink, &self.metrics].map(Items::digest)
        }
    }

    /// Every scalar's cmap result, then every glyph's advance and ink box, then the metrics:
    /// scalars as little-endian u32, glyph ids as little-endian u16, an option as its byte and
    /// then its little-endian u16 or i16 values.
    fn font_items(fonts: &Fonts, face: Face) -> FontItems {
        let mut cmap = Items::default();
        for c in scalars() {
            let out = cmap.item();
            out.extend_from_slice(&u32::from(c).to_le_bytes());
            opt(out, raw_glyph(fonts, face, c).map(u16::to_le_bytes));
        }
        let n = num_glyphs(fonts, face);
        let (mut advances, mut ink) = (Items::default(), Items::default());
        for gid in 0..n {
            let out = advances.item();
            out.extend_from_slice(&gid.to_le_bytes());
            opt(out, advance_units(fonts, face, gid).map(u16::to_le_bytes));
            let out = ink.item();
            out.extend_from_slice(&gid.to_le_bytes());
            opt(out, fonts.ink_bounds(face, gid).map(i16s));
        }
        let mut metrics = Items::default();
        let (upem, bounds, ascent, descent, cap) = face_metrics(fonts, face);
        let out = metrics.item();
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&upem.to_le_bytes());
        out.extend_from_slice(&i16s(bounds));
        for v in [ascent, descent, cap] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        FontItems {
            cmap,
            advances,
            ink,
            metrics,
        }
    }

    /// The sizes the public faces are compared at.
    const SIZES: [f32; 5] = [8.0, 10.0, 12.0, 18.0, 1000.0];
    const FACES: [Face; 5] = [
        Face::Sans,
        Face::Bold,
        Face::Italic,
        Face::BoldItalic,
        Face::Mono,
    ];

    /// The public face `face` at `size`: the size, ascent and descent, then every scalar's glyph
    /// and advance; each f32 as its little-endian `to_bits`.
    fn scaled_bytes(fonts: &Fonts, face: Face, size: f32) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 * 1_112_064 + 12);
        for v in [size, fonts.ascent(face, size), fonts.descent(face, size)] {
            out.extend_from_slice(&v.to_bits().to_le_bytes());
        }
        for c in scalars() {
            out.extend_from_slice(&u32::from(c).to_le_bytes());
            opt(&mut out, fonts.glyph(face, c).map(u16::to_le_bytes));
            out.extend_from_slice(&fonts.advance(face, c, size).to_bits().to_le_bytes());
        }
        out
    }

    /// The digest of each public face at every size: the digest of its sizes' digests, in
    /// order. The 25 face and size runs are independent, so each has its own thread.
    fn scaled_digests(fonts: &Fonts) -> Vec<(Face, String)> {
        std::thread::scope(|s| {
            let runs: Vec<_> = FACES
                .iter()
                .map(|face| {
                    let sizes: Vec<_> = SIZES
                        .iter()
                        .map(|size| {
                            s.spawn(move || {
                                Hash::of(&scaled_bytes(fonts, *face, *size)).to_string()
                            })
                        })
                        .collect();
                    (*face, sizes)
                })
                .collect();
            runs.into_iter()
                .map(|(face, sizes)| {
                    let all: String = sizes
                        .into_iter()
                        .map(|t| t.join().expect("a scaled run"))
                        .collect();
                    (face, Hash::of(all.as_bytes()).to_string())
                })
                .collect()
        })
    }

    /// The base reader's digests (ttf-parser 0.25.1, captured before the reader was replaced):
    /// per DejaVu font, [cmap, advances, ink boxes, metrics].
    const BASE_FONT_DIGESTS: [(&str, [&str; 4]); 2] = [
        (
            "Sans",
            [
                "45db985a60f9ea24020db97a5cc443cb2dc1b019bfb40b0acf8e23c0f33c4974",
                "9ce12db53976bfa32a30d2cc8564317ba95d0b08afc3626b6ac5594c9fefdd11",
                "915f40c3e04475947f772212ab49aaf97cc856441751b729e630d81f98bbe456",
                "5e6c5ba428ecd40ff6cfe202f7ead7d55ba137eccfdac51fbc8a4e759e4dc4b3",
            ],
        ),
        (
            "Bold",
            [
                "7e7995340374d65699978e849e6405bf3f7ad9e17ecc499819bdd246694be18c",
                "6fa42ead81bc1f581b5f19d89ff8986a7056d1a06edfaa14ff6a1db60436757f",
                "d5972310cf594f4370a128cc9bd6c20ff7249d7114f80b8a467cb49e033a4a27",
                "02212e0c6275adfe09985693f4ffdb17368edbe8eaeaa29ffe1cf5e7aed7f4be",
            ],
        ),
    ];
    /// The base reader's digests of the five public faces at every size in [`SIZES`].
    const BASE_SCALED_DIGESTS: [(Face, &str); 5] = [
        (
            Face::Sans,
            "3d8aac903000107869892c47415403ef8a2737c514d44cc82e45fadcb7046894",
        ),
        (
            Face::Bold,
            "1acc84c50f8273f9a05b85261c5b09728cbd87f43408f6398922e0cc73aae3d3",
        ),
        (
            Face::Italic,
            "3d8aac903000107869892c47415403ef8a2737c514d44cc82e45fadcb7046894",
        ),
        (
            Face::BoldItalic,
            "1acc84c50f8273f9a05b85261c5b09728cbd87f43408f6398922e0cc73aae3d3",
        ),
        (
            Face::Mono,
            "354e539b06d8941f7738514267cbb25c2a098b789be43089cb59e8b22d0d864c",
        ),
    ];

    #[test]
    fn plx1_every_embedded_font_mapping_and_metric_matches_the_base() {
        // the two compiled-in fonts' facts, read from their tables
        let fonts = Fonts::load();
        assert_eq!(scalars().count(), 1_112_064);
        let facts = [
            (Face::Sans, 6253, [-2090, -948, 3673, 2524]),
            (Face::Bold, 6196, [-2190, -850, 4045, 2407]),
        ];
        for (face, n, bounds) in facts {
            assert_eq!(num_glyphs(&fonts, face), n, "{face:?}");
            assert_eq!(
                face_metrics(&fonts, face),
                (2048, bounds, 1901, -483, 1901),
                "{face:?}"
            );
        }
        // every scalar, glyph and metric of both fonts, against the base reader's digests
        let got: Vec<(&str, [String; 4])> = [("Sans", Face::Sans), ("Bold", Face::Bold)]
            .into_iter()
            .map(|(name, face)| (name, font_items(&fonts, face).digests()))
            .collect();
        let got: Vec<(&str, [&str; 4])> = got
            .iter()
            .map(|(n, d)| (*n, [0, 1, 2, 3].map(|i| d[i].as_str())))
            .collect();
        assert_eq!(got, BASE_FONT_DIGESTS.to_vec(), "{got:#?}");
        // every public face's glyphs, advances and metrics at each size, bit for bit
        let got = scaled_digests(&fonts);
        let got: Vec<(Face, &str)> = got.iter().map(|(f, d)| (*f, d.as_str())).collect();
        assert_eq!(got, BASE_SCALED_DIGESTS.to_vec(), "{got:#?}");
    }

    /// SHA-256 of `data` (FIPS 180-4), so that the test checks the brand kit's own checksums with
    /// no new crate.
    fn sha256(data: &[u8]) -> String {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        let mut msg = data.to_vec();
        let bits = (data.len() as u64).wrapping_mul(8);
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bits.to_be_bytes());
        for chunk in msg.chunks(64) {
            let mut w = [0u32; 64];
            for (i, word) in chunk.chunks(4).enumerate() {
                w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ (!e & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
                *x = x.wrapping_add(y);
            }
        }
        h.iter().map(|x| format!("{x:08x}")).collect()
    }

    #[test]
    fn plx1_ink_bounds_stay_where_the_glyf_table_puts_the_points() {
        // O5 ruling (task-1, 2026-10-23): skrifa moves these outlines 1 unit along x by their
        // first phantom point; the old reader did not, and the bounds keep its frame. The values
        // are the base reader's, from the first differential run.
        let fonts = Fonts::load();
        assert_eq!(
            fonts.ink_bounds(Face::Sans, 1600),
            Some([-1185, 1308, -88, 1803])
        );
        assert_eq!(
            fonts.ink_bounds(Face::Bold, 710),
            Some([-182, 1262, 182, 1638])
        );
    }

    #[test]
    fn sha256_matches_the_standard_vectors() {
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn ec8_the_fonts_match_their_brand_kit_checksums() {
        // the brand kit's 05-Editable files (Task 0 Step 5, 2026-10-16)
        assert_eq!(SANS.len(), 759_720);
        assert_eq!(
            sha256(SANS),
            "ae7b7855e115a5966d8b1b3f80f254ccc117ec86f9965e202ee2940453837280"
        );
        assert_eq!(SANS_BOLD.len(), 708_920);
        assert_eq!(
            sha256(SANS_BOLD),
            "5c1247acef7f2b8522a31742c76d6adcb5569bacc0be7ceaa4dc39dd252ce895"
        );
    }

    #[test]
    fn ec8_sans_has_arrows_greek_and_math_symbols() {
        let fonts = Fonts::load();
        for face in [Face::Sans, Face::Bold, Face::Italic, Face::BoldItalic] {
            for c in ['→', '≤', 'α', '—', '…', 'A', 'é'] {
                let g = fonts.glyph(face, c);
                assert!(g.is_some_and(|g| g != 0), "{face:?} has no glyph for {c:?}");
                assert!(fonts.advance(face, c, 10.0) > 0.0, "{face:?} {c:?}");
            }
        }
        // Italic is Sans slanted: the same glyphs and advances
        assert_eq!(fonts.glyph(Face::Italic, '→'), fonts.glyph(Face::Sans, '→'));
        assert_eq!(
            fonts.advance(Face::BoldItalic, 'W', 12.0),
            fonts.advance(Face::Bold, 'W', 12.0)
        );
        // a size scales the advance
        let a = fonts.advance(Face::Sans, 'm', 10.0);
        assert!((fonts.advance(Face::Sans, 'm', 20.0) - 2.0 * a).abs() < 1e-4);
        // the metrics are the face's, scaled
        assert!(fonts.ascent(Face::Sans, 10.0) > 7.0);
        assert!(fonts.descent(Face::Sans, 10.0) < 0.0);
        // a character outside the face prints as `?`: its advance is `?`'s
        assert_eq!(fonts.glyph(Face::Sans, '中'), None);
        assert_eq!(
            fonts.advance(Face::Sans, '中', 10.0),
            fonts.advance(Face::Sans, '?', 10.0)
        );
    }

    #[test]
    fn ec8_a_script_that_needs_shaping_has_no_glyph_although_dejavu_draws_it() {
        // RULING F1: the writer neither shapes nor lays text out right to left, so the scripts
        // that need either print as `?`, even where DejaVu holds their letters (final review I1)
        let fonts = Fonts::load();
        let shaped = [
            'ع',         // Arabic
            'ش',         // Arabic
            '\u{0660}',  // Arabic-Indic digit zero
            'ש',         // Hebrew
            '\u{07CA}',  // NKo
            'ກ',         // Lao
            '\u{FB52}',  // Arabic presentation form A
            '\u{FEC9}',  // Arabic presentation form B
            '\u{FB2A}',  // Hebrew presentation form
            '\u{1EE01}', // Arabic mathematical letter
        ];
        for c in shaped {
            assert!(
                fonts.sans.glyph(c).is_some(),
                "DejaVu Sans draws {c:?}: the test must name a letter it holds"
            );
            for face in [Face::Sans, Face::Bold, Face::Italic, Face::BoldItalic] {
                assert_eq!(fonts.glyph(face, c), None, "{face:?} {c:?}");
                assert_eq!(
                    fonts.advance(face, c, 10.0),
                    fonts.advance(face, '?', 10.0),
                    "{face:?} {c:?} takes `?`'s place"
                );
            }
        }
        // the neighbours print: Armenian, Georgian, Greek, Cyrillic, Thai's baht sign, the
        // Latin ligatures and the Armenian ones before the Hebrew presentation forms
        for c in ['Ա', 'ა', 'α', 'Ж', '฿', '\u{FB01}', '\u{FB13}', '\u{058A}'] {
            assert!(fonts.glyph(Face::Sans, c).is_some(), "{c:?}");
        }
    }

    #[test]
    fn ec8_mono_maps_only_windows_1252() {
        let fonts = Fonts::load();
        // ASCII and Latin-1 map to their own code
        assert_eq!(fonts.glyph(Face::Mono, 'A'), Some(0x41));
        assert_eq!(fonts.glyph(Face::Mono, ' '), Some(0x20));
        assert_eq!(fonts.glyph(Face::Mono, '~'), Some(0x7e));
        assert_eq!(fonts.glyph(Face::Mono, 'é'), Some(0xe9));
        assert_eq!(fonts.glyph(Face::Mono, 'ÿ'), Some(0xff));
        // Windows-1252's own row 0x80–0x9f
        assert_eq!(fonts.glyph(Face::Mono, '€'), Some(0x80));
        assert_eq!(fonts.glyph(Face::Mono, '…'), Some(0x85));
        assert_eq!(fonts.glyph(Face::Mono, '—'), Some(0x97));
        assert_eq!(fonts.glyph(Face::Mono, 'Ÿ'), Some(0x9f));
        // nothing else
        for c in [
            '→', '≤', 'α', '中', '\u{81}', '\u{9d}', '\t', '\n', '\u{7f}',
        ] {
            assert_eq!(fonts.glyph(Face::Mono, c), None, "{c:?}");
        }
        // Courier: every glyph is 600 units wide
        assert!((fonts.advance(Face::Mono, 'i', 10.0) - 6.0).abs() < 1e-4);
        assert!((fonts.advance(Face::Mono, 'W', 9.0) - 5.4).abs() < 1e-4);
        assert!((fonts.advance(Face::Mono, '→', 9.0) - 5.4).abs() < 1e-4);
        assert!(fonts.ascent(Face::Mono, 10.0) > 0.0);
        assert!(fonts.descent(Face::Mono, 10.0) < 0.0);
    }
}
