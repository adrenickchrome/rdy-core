//! The PDF's fonts (RULING F1): DejaVu Sans and DejaVu Sans Bold from the brand kit, compiled in,
//! and Courier, the PDF standard font, for code. `ttf-parser` reads the DejaVu metrics; Courier's
//! are its AFM's (every glyph 600 units wide).
//!
//! Italic is Sans slanted 12 degrees, and Bold Italic is Bold slanted the same way: the PDF writer
//! slants them with the text matrix, so they share their upright face's glyphs and advances.
//! Courier is written with WinAnsiEncoding, so its glyph for a character is the character's
//! Windows-1252 code, and a character outside Windows-1252 has none.

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

/// The two parsed DejaVu faces.
pub struct Fonts {
    sans: ttf_parser::Face<'static>,
    bold: ttf_parser::Face<'static>,
}

impl Fonts {
    /// Parses the two compiled-in faces.
    pub fn load() -> Fonts {
        let parse = |bytes: &'static [u8]| {
            ttf_parser::Face::parse(bytes, 0).expect("the compiled-in DejaVu font parses")
        };
        Fonts {
            sans: parse(SANS),
            bold: parse(SANS_BOLD),
        }
    }

    /// The DejaVu face `face` draws with; `None` for Courier.
    fn ttf(&self, face: Face) -> Option<&ttf_parser::Face<'static>> {
        match face {
            Face::Sans | Face::Italic => Some(&self.sans),
            Face::Bold | Face::BoldItalic => Some(&self.bold),
            Face::Mono => None,
        }
    }

    /// The glyph for `c` in `face`; None when the face has none (Mono: outside Windows-1252).
    pub fn glyph(&self, face: Face, c: char) -> Option<u16> {
        match self.ttf(face) {
            Some(f) => f.glyph_index(c).map(|g| g.0).filter(|g| *g != 0),
            None => cp1252(c).map(u16::from),
        }
    }

    /// The advance of `c` at `size` points (`?`'s advance when there is no glyph).
    pub fn advance(&self, face: Face, c: char, size: f32) -> f32 {
        let Some(f) = self.ttf(face) else {
            return COURIER_ADVANCE * size / 1000.0;
        };
        let g = self
            .glyph(face, c)
            .or_else(|| self.glyph(face, '?'))
            .unwrap_or(0);
        let units = f.glyph_hor_advance(ttf_parser::GlyphId(g)).unwrap_or(0);
        f32::from(units) * size / f32::from(f.units_per_em())
    }

    /// How far the face rises above the baseline at `size` points.
    pub fn ascent(&self, face: Face, size: f32) -> f32 {
        match self.ttf(face) {
            Some(f) => f32::from(f.ascender()) * size / f32::from(f.units_per_em()),
            None => COURIER_ASCENT * size / 1000.0,
        }
    }

    /// How far the face falls below the baseline at `size` points, as a negative number (the
    /// font's own sign, and the PDF's).
    pub fn descent(&self, face: Face, size: f32) -> f32 {
        match self.ttf(face) {
            Some(f) => f32::from(f.descender()) * size / f32::from(f.units_per_em()),
            None => COURIER_DESCENT * size / 1000.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
