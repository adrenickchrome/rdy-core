//! The figure images a PDF prints (D-6SANB, O33): JPEG passed through, and PNG decoded to its
//! samples. The list is closed: a figure of any other type, or of another form, does not print,
//! and the PDF writer leaves a line and a warning in its place.

/// A figure's samples, as the PDF writer embeds them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pixels {
    /// A JPEG's own bytes, which the PDF holds as they are (DCTDecode).
    Jpeg {
        w: u32,
        h: u32,
        components: u8,
        bytes: Vec<u8>,
    },
    /// 8-bit samples, row by row: one a pixel when `gray`, else three (RGB). `alpha` is one
    /// sample a pixel, which the PDF holds as a soft mask.
    Raw {
        w: u32,
        h: u32,
        gray: bool,
        color: Vec<u8>,
        alpha: Option<Vec<u8>>,
    },
}

impl Pixels {
    /// The image's width and height in pixels.
    pub fn size(&self) -> (u32, u32) {
        match self {
            Pixels::Jpeg { w, h, .. } | Pixels::Raw { w, h, .. } => (*w, *h),
        }
    }
}

/// The widest and the tallest image a PDF export prints, in pixels.
pub const MAX_SIDE: u32 = 8192;
/// The most bytes a PNG's decoded samples take in a PDF export (64 MiB).
pub const MAX_DECODED: u64 = 64 * 1024 * 1024;

/// The image types that print (closed): JPEG (baseline or progressive; passed as DCTDecode) with 1
/// component (DeviceGray) or 3 (DeviceRGB) (O33), and
/// PNG of bit depth 8, colour type gray, RGB, gray with alpha or RGBA, not interlaced (decoded with
/// miniz_oxide; alpha becomes a soft mask). Anything else is None.
///
/// The budget (the task-9 ruling): an image wider or taller than [`MAX_SIDE`], or a PNG whose
/// samples take more than [`MAX_DECODED`] bytes, is None too. This is checked from the header,
/// before any inflation or allocation; [`over_budget`] names such an image's size. A JPEG prints
/// only when its marker structure is whole: one frame header, a scan with data, and an end.
/// A gray or RGB PNG's tRNS colour key becomes a soft mask.
pub fn decode(media_type: &str, bytes: &[u8]) -> Option<Pixels> {
    match media_type {
        "image/jpeg" => {
            let (w, h, components) = jpeg_frame(bytes)?;
            fits(w, h, None).then(|| Pixels::Jpeg {
                w,
                h,
                components,
                bytes: bytes.to_vec(),
            })
        }
        "image/png" => png(bytes),
        _ => None,
    }
}

/// The size of an image of a form that prints but that is larger than a PDF export prints (the
/// task-9 ruling's budget). None for an image in the budget, an image of another type or form, a
/// zero side, or a damaged image: those are [`decode`]'s None.
pub fn over_budget(media_type: &str, bytes: &[u8]) -> Option<(u32, u32)> {
    let (w, h, channels) = match media_type {
        "image/jpeg" => jpeg_frame(bytes).map(|(w, h, _)| (w, h, None))?,
        "image/png" => {
            let (body, kind) = png_chunks(bytes)?.next()?;
            if &kind != b"IHDR" {
                return None;
            }
            let (w, h, channels) = png_header(body?)?;
            (w, h, Some(channels))
        }
        _ => return None,
    };
    (!fits(w, h, channels)).then_some((w, h))
}

/// Whether an image of `w` × `h` (with `channels` samples a pixel, for a PNG, which is decoded)
/// is in the budget.
fn fits(w: u32, h: u32, channels: Option<u8>) -> bool {
    w <= MAX_SIDE
        && h <= MAX_SIDE
        && channels.is_none_or(|c| u64::from(w) * u64::from(h) * u64::from(c) <= MAX_DECODED)
}

/// A JPEG's size and components, from its frame header, when its marker structure is whole:
/// SOI; segments whose lengths fit the file; exactly one frame header, baseline (SOF0) or
/// progressive (SOF2), of 8-bit samples, a non-zero size and 1 or 3 components, whose length
/// matches its components; at least one scan after it, each with a header whose length matches
/// its 1–4 components, and at least one with entropy-coded data; and EOI. Bytes after EOI are not
/// read.
fn jpeg_frame(bytes: &[u8]) -> Option<(u32, u32, u8)> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut frame: Option<(u32, u32, u8)> = None;
    let mut scan_data = false;
    let mut i = 2;
    loop {
        // a marker, after any fill bytes
        if *bytes.get(i)? != 0xff {
            return None;
        }
        while *bytes.get(i)? == 0xff {
            i += 1;
        }
        let marker = *bytes.get(i)?;
        i += 1;
        match marker {
            // markers with no segment
            0x01 | 0xd0..=0xd7 => continue,
            // the end: the image is whole when it has a frame and a scan with data
            0xd9 => return frame.filter(|_| scan_data),
            // a second start
            0xd8 => return None,
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes([*bytes.get(i)?, *bytes.get(i + 1)?]));
        if len < 2 {
            return None;
        }
        let seg = bytes.get(i + 2..i + len)?;
        i += len;
        match marker {
            0xc0 | 0xc2 => {
                let [precision, h0, h1, w0, w1, components, ref specs @ ..] = *seg else {
                    return None;
                };
                let (w, h) = (
                    u32::from(u16::from_be_bytes([w0, w1])),
                    u32::from(u16::from_be_bytes([h0, h1])),
                );
                if frame.is_some()
                    || precision != 8
                    || w == 0
                    || h == 0
                    || !matches!(components, 1 | 3)
                    || specs.len() != 3 * usize::from(components)
                {
                    return None;
                }
                frame = Some((w, h, components));
            }
            // every other frame type: extended, lossless, hierarchical or arithmetic-coded
            0xc1 | 0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => return None,
            0xda => {
                let (_, _, components) = frame?;
                let [ns, ref rest @ ..] = *seg else {
                    return None;
                };
                if !(1..=4).contains(&ns)
                    || ns > components
                    || rest.len() != 2 * usize::from(ns) + 3
                {
                    return None;
                }
                // the entropy-coded data runs to the next marker that is not a stuffed 0xFF or a
                // restart marker
                let start = i;
                loop {
                    if *bytes.get(i)? == 0xff {
                        match *bytes.get(i + 1)? {
                            0x00 | 0xd0..=0xd7 => i += 2,
                            _ => break,
                        }
                    } else {
                        i += 1;
                    }
                }
                scan_data |= i > start;
            }
            _ => {}
        }
    }
}

/// A PNG's chunks after its signature, each as its body (None when its length passes the end of
/// the file) and its kind. The chunks' CRCs are not checked.
fn png_chunks(bytes: &[u8]) -> Option<impl Iterator<Item = (Option<&[u8]>, [u8; 4])>> {
    let mut rest = Some(bytes.strip_prefix(b"\x89PNG\r\n\x1a\n")?);
    Some(std::iter::from_fn(move || {
        let r = rest.take()?;
        if r.len() < 12 {
            return None;
        }
        let len = usize::try_from(u32::from_be_bytes(r[..4].try_into().ok()?)).ok()?;
        let kind: [u8; 4] = r[4..8].try_into().ok()?;
        let body = 8usize.checked_add(len).and_then(|end| r.get(8..end));
        rest = 12usize.checked_add(len).and_then(|end| r.get(end..));
        Some((body, kind))
    }))
}

/// A PNG header of a form that prints: its size and its samples a pixel.
fn png_header(ihdr: &[u8]) -> Option<(u32, u32, u8)> {
    let [
        w0,
        w1,
        w2,
        w3,
        h0,
        h1,
        h2,
        h3,
        depth,
        color,
        compression,
        filter,
        interlace,
    ] = *ihdr
    else {
        return None;
    };
    let (w, h) = (
        u32::from_be_bytes([w0, w1, w2, w3]),
        u32::from_be_bytes([h0, h1, h2, h3]),
    );
    let channels = match color {
        0 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return None,
    };
    if depth != 8 || compression != 0 || filter != 0 || interlace != 0 || w == 0 || h == 0 {
        return None;
    }
    Some((w, h, channels))
}

/// Inflates a zlib stream that must give exactly `size` bytes. Inflation stops past `size`, so
/// the output is never larger than the header asks for.
fn inflate(data: &[u8], size: usize) -> Option<Vec<u8>> {
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, size).ok()?;
    (raw.len() == size).then_some(raw)
}

/// A PNG's samples: bit depth 8, colour type 0 (gray), 2 (RGB), 4 (gray and alpha) or 6 (RGBA),
/// no interlace, in the budget. The header is the first chunk and is checked before any data is
/// read; the data must inflate to exactly the image. A gray or RGB image's tRNS colour key gives
/// an alpha of 0 where a pixel is the key and 255 elsewhere; a tRNS of another length is not a
/// key.
fn png(bytes: &[u8]) -> Option<Pixels> {
    let mut chunks = png_chunks(bytes)?;
    let (ihdr, kind) = chunks.next()?;
    if &kind != b"IHDR" {
        return None;
    }
    let (w, h, channels) = png_header(ihdr?)?;
    if !fits(w, h, Some(channels)) {
        return None;
    }
    let mut data = vec![];
    let mut key: Option<&[u8]> = None;
    for (body, kind) in chunks {
        let body = body?;
        match &kind {
            b"IDAT" => data.extend_from_slice(body),
            b"tRNS" => key = Some(body),
            b"IEND" => break,
            _ => {}
        }
    }
    let channels = usize::from(channels);
    let (wu, hu) = (usize::try_from(w).ok()?, usize::try_from(h).ok()?);
    let stride = wu.checked_mul(channels)?;
    let raw = inflate(&data, stride.checked_add(1)?.checked_mul(hu)?)?;
    drop(data);
    let mut samples = unfilter(&raw, stride, channels)?;
    drop(raw);
    let alpha = match (channels, key) {
        (1, Some(&[k0, k1])) => {
            let k = u16::from_be_bytes([k0, k1]);
            Some(
                samples
                    .iter()
                    .map(|s| key_alpha(u16::from(*s) == k))
                    .collect(),
            )
        }
        (3, Some(&[r0, r1, g0, g1, b0, b1])) => {
            let k = [
                u16::from_be_bytes([r0, r1]),
                u16::from_be_bytes([g0, g1]),
                u16::from_be_bytes([b0, b1]),
            ];
            Some(
                samples
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|px| key_alpha(px.map(u16::from) == k))
                    .collect(),
            )
        }
        (1 | 3, _) => None,
        _ => {
            // the alpha channel out, and the colour samples moved down in place
            let c = channels - 1;
            let mut alpha = Vec::with_capacity(wu * hu);
            for px in 0..wu * hu {
                alpha.push(samples[px * channels + c]);
                samples.copy_within(px * channels..px * channels + c, px * c);
            }
            samples.truncate(wu * hu * c);
            Some(alpha)
        }
    };
    Some(Pixels::Raw {
        w,
        h,
        gray: matches!(channels, 1 | 2),
        color: samples,
        alpha,
    })
}

/// The alpha of a pixel that is, or is not, its image's tRNS colour key.
fn key_alpha(is_key: bool) -> u8 {
    if is_key { 0 } else { 255 }
}

/// The samples of PNG scanlines of `stride` bytes, each led by its filter type (0–4), with
/// `bpp` bytes a pixel.
fn unfilter(raw: &[u8], stride: usize, bpp: usize) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(raw.len() / (stride + 1) * stride);
    for (y, line) in raw.chunks_exact(stride + 1).enumerate() {
        let ft = line[0];
        let start = y * stride;
        for i in 0..stride {
            let a = if i >= bpp { out[start + i - bpp] } else { 0 };
            let b = if y > 0 { out[start - stride + i] } else { 0 };
            let c = if y > 0 && i >= bpp {
                out[start - stride + i - bpp]
            } else {
                0
            };
            let pred = match ft {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                4 => paeth(a, b, c),
                _ => return None,
            };
            out.push(line[1 + i].wrapping_add(pred));
        }
    }
    Some(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = (
        (p - i16::from(a)).abs(),
        (p - i16::from(b)).abs(),
        (p - i16::from(c)).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

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

    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }

    /// A PNG of `w` × `h` with this header and these filtered scanlines.
    pub(crate) fn png(
        w: u32,
        h: u32,
        depth: u8,
        color: u8,
        interlace: u8,
        filtered: &[u8],
    ) -> Vec<u8> {
        png_with(w, h, depth, color, interlace, filtered, &[])
    }

    pub(crate) fn png_with(
        w: u32,
        h: u32,
        depth: u8,
        color: u8,
        interlace: u8,
        filtered: &[u8],
        before_idat: &[(&[u8; 4], Vec<u8>)],
    ) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = vec![];
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[depth, color, 0, 0, interlace]);
        chunk(&mut out, b"IHDR", &ihdr);
        for (kind, data) in before_idat {
            chunk(&mut out, kind, data);
        }
        let z = miniz_oxide::deflate::compress_to_vec_zlib(filtered, 6);
        // the data in two IDAT chunks, which a decoder joins
        let (a, b) = z.split_at(z.len() / 2);
        chunk(&mut out, b"IDAT", a);
        chunk(&mut out, b"IDAT", b);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    fn paeth(a: u8, b: u8, c: u8) -> u8 {
        let p = i16::from(a) + i16::from(b) - i16::from(c);
        let (pa, pb, pc) = (
            (p - i16::from(a)).abs(),
            (p - i16::from(b)).abs(),
            (p - i16::from(c)).abs(),
        );
        if pa <= pb && pa <= pc {
            a
        } else if pb <= pc {
            b
        } else {
            c
        }
    }

    /// The scanlines of `rows` (each `bpp` bytes a pixel), row `y` filtered with filter type
    /// `y % 5`, so every filter is met.
    pub(crate) fn filter(rows: &[Vec<u8>], bpp: usize) -> Vec<u8> {
        let mut out = vec![];
        let zero = vec![0u8; rows[0].len()];
        for (y, row) in rows.iter().enumerate() {
            let prior = if y == 0 { &zero } else { &rows[y - 1] };
            let ft = (y % 5) as u8;
            out.push(ft);
            for i in 0..row.len() {
                let a = if i >= bpp { row[i - bpp] } else { 0 };
                let b = prior[i];
                let c = if i >= bpp { prior[i - bpp] } else { 0 };
                let pred = match ft {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                    _ => paeth(a, b, c),
                };
                out.push(row[i].wrapping_sub(pred));
            }
        }
        out
    }

    /// `h` rows of `w` pixels of `bpp` bytes, with values that differ from pixel to pixel.
    pub(crate) fn pixels(w: usize, h: usize, bpp: usize) -> Vec<Vec<u8>> {
        (0..h)
            .map(|y| {
                (0..w * bpp)
                    .map(|i| ((i * 53 + y * 97 + (i * y) % 7 * 31) % 256) as u8)
                    .collect()
            })
            .collect()
    }

    #[test]
    fn ec9_png_rgba_splits_into_color_and_alpha() {
        let (w, h) = (3usize, 5usize);
        let rows = pixels(w, h, 4);
        let bytes = png(w as u32, h as u32, 8, 6, 0, &filter(&rows, 4));
        let Some(Pixels::Raw {
            w: dw,
            h: dh,
            gray,
            color,
            alpha,
        }) = decode("image/png", &bytes)
        else {
            panic!("an RGBA PNG decodes");
        };
        assert_eq!((dw, dh, gray), (3, 5, false));
        let flat: Vec<u8> = rows.concat();
        let want_color: Vec<u8> = flat
            .chunks(4)
            .flat_map(|p| p[..3].to_vec())
            .collect::<Vec<_>>();
        let want_alpha: Vec<u8> = flat.chunks(4).map(|p| p[3]).collect();
        assert_eq!(color, want_color);
        assert_eq!(alpha, Some(want_alpha));

        // gray with alpha splits the same way
        let rows = pixels(4, 5, 2);
        let bytes = png(4, 5, 8, 4, 0, &filter(&rows, 2));
        let Some(Pixels::Raw {
            gray, color, alpha, ..
        }) = decode("image/png", &bytes)
        else {
            panic!("a gray-with-alpha PNG decodes");
        };
        let flat = rows.concat();
        assert!(gray);
        assert_eq!(color, flat.chunks(2).map(|p| p[0]).collect::<Vec<_>>());
        assert_eq!(alpha, Some(flat.chunks(2).map(|p| p[1]).collect()));

        // RGB and gray have no alpha
        let rows = pixels(2, 5, 3);
        let bytes = png(2, 5, 8, 2, 0, &filter(&rows, 3));
        assert_eq!(
            decode("image/png", &bytes),
            Some(Pixels::Raw {
                w: 2,
                h: 5,
                gray: false,
                color: rows.concat(),
                alpha: None
            })
        );
        let rows = pixels(7, 5, 1);
        let bytes = png(7, 5, 8, 0, 0, &filter(&rows, 1));
        assert_eq!(
            decode("image/png", &bytes),
            Some(Pixels::Raw {
                w: 7,
                h: 5,
                gray: true,
                color: rows.concat(),
                alpha: None
            })
        );
        assert_eq!(decode("image/png", &bytes).unwrap().size(), (7, 5));
    }

    #[test]
    fn ec9_png_of_another_form_does_not_decode() {
        // the guard: the same image as 8-bit RGB decodes
        let rows = pixels(2, 2, 3);
        assert!(decode("image/png", &png(2, 2, 8, 2, 0, &filter(&rows, 3))).is_some());
        // 16-bit
        let rows16 = pixels(2, 2, 6);
        assert_eq!(
            decode("image/png", &png(2, 2, 16, 2, 0, &filter(&rows16, 6))),
            None
        );
        // palette
        let idx = vec![vec![0u8, 1], vec![1, 0]];
        let plte = vec![(b"PLTE", vec![0u8, 0, 0, 255, 255, 255])];
        assert_eq!(
            decode(
                "image/png",
                &png_with(2, 2, 8, 3, 0, &filter(&idx, 1), &plte)
            ),
            None
        );
        // interlaced (Adam7)
        assert_eq!(
            decode("image/png", &png(2, 2, 8, 2, 1, &filter(&rows, 3))),
            None
        );
        // a bit depth below 8
        assert_eq!(
            decode("image/png", &png(8, 2, 1, 0, 0, &[0, 0xaa, 0, 0x55])),
            None
        );
        // data that is short of the image
        assert_eq!(
            decode("image/png", &png(2, 3, 8, 2, 0, &filter(&rows, 3))),
            None
        );
        // a filter type outside 0–4
        let mut bad = filter(&rows, 3);
        bad[0] = 5;
        assert_eq!(decode("image/png", &png(2, 2, 8, 2, 0, &bad)), None);
        // not a PNG, or a PNG under another media type
        assert_eq!(decode("image/png", b"png"), None);
        let good = png(2, 2, 8, 2, 0, &filter(&rows, 3));
        for media in [
            "image/gif",
            "image/svg+xml",
            "image/webp",
            "application/png",
        ] {
            assert_eq!(decode(media, &good), None, "{media}");
        }
    }

    /// Real JPEGs of 16 × 8 pixels: baseline and progressive, RGB and gray.
    pub(crate) const BASELINE_RGB: &[u8] = include_bytes!("testdata/baseline-rgb.jpg");
    pub(crate) const BASELINE_GRAY: &[u8] = include_bytes!("testdata/baseline-gray.jpg");
    pub(crate) const PROGRESSIVE_RGB: &[u8] = include_bytes!("testdata/progressive-rgb.jpg");
    pub(crate) const PROGRESSIVE_GRAY: &[u8] = include_bytes!("testdata/progressive-gray.jpg");

    /// The offset just past the first segment with this marker.
    pub(crate) fn after_segment(bytes: &[u8], marker: u8) -> usize {
        let mut i = 2;
        loop {
            assert_eq!(bytes[i], 0xff, "a marker at {i}");
            let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
            if bytes[i + 1] == marker {
                return i + 2 + len;
            }
            i += 2 + len;
        }
    }

    /// A JPEG's marker structure with this frame header: APP0, the frame, one scan of
    /// `components` with two bytes of entropy-coded data (one a stuffed 0xFF), and EOI. It has no
    /// tables, so it does not render, but its structure is whole.
    fn jpeg(sof: u8, precision: u8, w: u16, h: u16, components: u8) -> Vec<u8> {
        let mut out = vec![0xff, 0xd8];
        out.extend_from_slice(&[0xff, 0xe0, 0x00, 0x07, b'J', b'F', b'I', b'F', 0]);
        let len = 8 + 3 * u16::from(components);
        out.extend_from_slice(&[0xff, sof]);
        out.extend_from_slice(&len.to_be_bytes());
        out.push(precision);
        out.extend_from_slice(&h.to_be_bytes());
        out.extend_from_slice(&w.to_be_bytes());
        out.push(components);
        for c in 0..components {
            out.extend_from_slice(&[c + 1, 0x11, 0]);
        }
        let ns = components.min(4);
        out.extend_from_slice(&[0xff, 0xda]);
        out.extend_from_slice(&(6 + 2 * u16::from(ns)).to_be_bytes());
        out.push(ns);
        for c in 0..ns {
            out.extend_from_slice(&[c + 1, 0x00]);
        }
        out.extend_from_slice(&[0, 63, 0]);
        out.extend_from_slice(&[0x12, 0xff, 0x00, 0x34]);
        out.extend_from_slice(&[0xff, 0xd9]);
        out
    }

    #[test]
    fn ec9_a_jpeg_passes_through_with_its_size() {
        for (name, bytes, components) in [
            ("baseline RGB", BASELINE_RGB, 3),
            ("baseline gray", BASELINE_GRAY, 1),
            ("progressive RGB", PROGRESSIVE_RGB, 3),
            ("progressive gray", PROGRESSIVE_GRAY, 1),
        ] {
            assert_eq!(
                decode("image/jpeg", bytes),
                Some(Pixels::Jpeg {
                    w: 16,
                    h: 8,
                    components,
                    bytes: bytes.to_vec()
                }),
                "{name}"
            );
        }
        // the guard: the synthetic structure decodes as a baseline and a progressive frame
        for sof in [0xc0, 0xc2] {
            assert!(decode("image/jpeg", &jpeg(sof, 8, 4, 4, 3)).is_some());
        }
        // CMYK, 12-bit, lossless and arithmetic-coded frames do not print (O33)
        assert_eq!(decode("image/jpeg", &jpeg(0xc0, 8, 4, 4, 4)), None);
        assert_eq!(decode("image/jpeg", &jpeg(0xc0, 12, 4, 4, 3)), None);
        assert_eq!(decode("image/jpeg", &jpeg(0xc3, 8, 4, 4, 3)), None);
        assert_eq!(decode("image/jpeg", &jpeg(0xc9, 8, 4, 4, 3)), None);
        assert_eq!(decode("image/jpeg", &jpeg(0xc0, 8, 0, 4, 3)), None);
        assert_eq!(decode("image/jpeg", b"\xff\xd8\xff"), None);
        assert_eq!(decode("image/png", BASELINE_RGB), None);
    }

    #[test]
    fn ec9_a_jpeg_cut_short_does_not_decode() {
        // the finding's case: an ordinary baseline JPEG cut just after its frame header
        let cut = &BASELINE_RGB[..after_segment(BASELINE_RGB, 0xc0)];
        assert_eq!(decode("image/jpeg", cut), None, "cut after SOF0");
        // every shorter prefix of each real JPEG: none has its scan and its end
        for (name, bytes) in [
            ("baseline RGB", BASELINE_RGB),
            ("baseline gray", BASELINE_GRAY),
            ("progressive RGB", PROGRESSIVE_RGB),
            ("progressive gray", PROGRESSIVE_GRAY),
        ] {
            for n in 0..bytes.len() {
                assert_eq!(decode("image/jpeg", &bytes[..n]), None, "{name} cut at {n}");
            }
        }
        let good = jpeg(0xc0, 8, 4, 4, 3);
        let sof = good.iter().position(|b| *b == 0xc0).unwrap() - 1;
        let sos = good.windows(2).position(|w| w == [0xff, 0xda]).unwrap();
        // a frame header whose length does not match its components
        let mut bad = good.clone();
        bad[sof + 3] += 3;
        assert_eq!(decode("image/jpeg", &bad), None, "frame length");
        let mut bad = good.clone();
        bad[sof + 9] = 2;
        assert_eq!(decode("image/jpeg", &bad), None, "frame components");
        // a scan header whose length does not match its components, or with none
        let mut bad = good.clone();
        bad[sos + 4] = 2;
        assert_eq!(decode("image/jpeg", &bad), None, "scan components");
        let mut bad = good.clone();
        bad[sos + 4] = 0;
        assert_eq!(decode("image/jpeg", &bad), None, "a scan of no component");
        // a scan before the frame
        let mut bad = good[..sof].to_vec();
        bad.extend_from_slice(&good[sos..good.len() - 2]);
        bad.extend_from_slice(&good[sof..sos]);
        bad.extend_from_slice(&[0xff, 0xd9]);
        assert_eq!(decode("image/jpeg", &bad), None, "scan before frame");
        // no scan at all, but an end
        let mut bad = good[..sos].to_vec();
        bad.extend_from_slice(&[0xff, 0xd9]);
        assert_eq!(decode("image/jpeg", &bad), None, "no scan");
        // a scan with no entropy-coded data
        let mut bad = good[..sos + 2 + 8].to_vec();
        bad.extend_from_slice(&[0xff, 0xd9]);
        assert_eq!(decode("image/jpeg", &bad), None, "an empty scan");
        // two frame headers
        let mut bad = good[..sos].to_vec();
        bad.extend_from_slice(&good[sof..sos]);
        bad.extend_from_slice(&good[sos..]);
        assert_eq!(decode("image/jpeg", &bad), None, "two frames");
        // bytes after the end are not read
        let mut more = BASELINE_GRAY.to_vec();
        more.extend_from_slice(b"trailing");
        assert!(decode("image/jpeg", &more).is_some());
    }

    #[test]
    fn ec9_png_gray_and_rgb_take_alpha_from_their_trns_key() {
        // RGB: the key (10, 20, 30) is transparent, every other colour opaque
        let rows = vec![
            vec![10u8, 20, 30, 10, 20, 31, 0, 0, 0],
            vec![10, 20, 30, 1, 2, 3, 10, 20, 30],
        ];
        let trns = vec![(b"tRNS", vec![0u8, 10, 0, 20, 0, 30])];
        let bytes = png_with(3, 2, 8, 2, 0, &filter(&rows, 3), &trns);
        assert_eq!(
            decode("image/png", &bytes),
            Some(Pixels::Raw {
                w: 3,
                h: 2,
                gray: false,
                color: rows.concat(),
                alpha: Some(vec![0, 255, 255, 0, 255, 0]),
            })
        );
        // gray: the key 5
        let rows = vec![vec![5u8, 6, 5, 0], vec![255, 5, 5, 7]];
        let trns = vec![(b"tRNS", vec![0u8, 5])];
        let bytes = png_with(4, 2, 8, 0, 0, &filter(&rows, 1), &trns);
        assert_eq!(
            decode("image/png", &bytes),
            Some(Pixels::Raw {
                w: 4,
                h: 2,
                gray: true,
                color: rows.concat(),
                alpha: Some(vec![0, 255, 0, 255, 255, 0, 0, 255]),
            })
        );
        // a gray key above 255 matches no 8-bit sample
        let trns = vec![(b"tRNS", vec![1u8, 5])];
        let bytes = png_with(4, 2, 8, 0, 0, &filter(&rows, 1), &trns);
        let Some(Pixels::Raw { alpha, .. }) = decode("image/png", &bytes) else {
            panic!("decodes");
        };
        assert_eq!(alpha, Some(vec![255; 8]));
        // a tRNS of the wrong length is not a key
        let trns = vec![(b"tRNS", vec![0u8, 5, 0])];
        let bytes = png_with(4, 2, 8, 0, 0, &filter(&rows, 1), &trns);
        let Some(Pixels::Raw { alpha, .. }) = decode("image/png", &bytes) else {
            panic!("decodes");
        };
        assert_eq!(alpha, None);
        // an image with an alpha channel keeps it
        let rows = pixels(2, 2, 4);
        let trns = vec![(b"tRNS", vec![0u8, 1, 0, 2, 0, 3])];
        let bytes = png_with(2, 2, 8, 6, 0, &filter(&rows, 4), &trns);
        let Some(Pixels::Raw { alpha, .. }) = decode("image/png", &bytes) else {
            panic!("decodes");
        };
        assert_eq!(alpha, Some(rows.concat().chunks(4).map(|p| p[3]).collect()));
    }

    /// A PNG with this header and an IDAT that is not zlib data: nothing past the header is
    /// read when the header is over the budget.
    fn png_header_only(w: u32, h: u32, color: u8) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = vec![];
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, color, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", b"not zlib");
        chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn ec9_an_image_over_the_pixel_budget_does_not_decode() {
        // the guard: 8192 × 1 gray decodes
        let wide = |w: usize| {
            let mut line = vec![0u8];
            line.extend(std::iter::repeat_n(7u8, w));
            line
        };
        assert!(decode("image/png", &png(8192, 1, 8, 0, 0, &wide(8192))).is_some());
        assert_eq!(
            over_budget("image/png", &png(8192, 1, 8, 0, 0, &wide(8192))),
            None
        );
        // 8193 × 1, whole and valid, is over the side
        let over = png(8193, 1, 8, 0, 0, &wide(8193));
        assert_eq!(decode("image/png", &over), None);
        assert_eq!(over_budget("image/png", &over), Some((8193, 1)));
        assert_eq!(
            over_budget("image/png", &png_header_only(1, 8193, 0)),
            Some((1, 8193))
        );
        // 5000 × 5000 RGBA is 100,000,000 bytes decoded, over 64 MiB
        assert_eq!(
            over_budget("image/png", &png_header_only(5000, 5000, 6)),
            Some((5000, 5000))
        );
        assert_eq!(decode("image/png", &png_header_only(5000, 5000, 6)), None);
        // 64 MiB exactly is in the budget; one row more is not
        assert_eq!(
            over_budget("image/png", &png_header_only(8192, 2048, 6)),
            None
        );
        assert_eq!(
            over_budget("image/png", &png_header_only(8192, 2049, 6)),
            Some((8192, 2049))
        );
        assert_eq!(
            over_budget("image/png", &png_header_only(4096, 4096, 6)),
            None
        );
        assert_eq!(
            over_budget("image/png", &png_header_only(8192, 8192, 0)),
            None
        );
        assert_eq!(
            over_budget("image/png", &png_header_only(8192, 8193, 0)),
            Some((8192, 8193))
        );
        // a zero side or a form that does not print is not over the budget: it does not decode
        assert_eq!(over_budget("image/png", &png_header_only(0, 9000, 0)), None);
        assert_eq!(
            over_budget("image/png", &png_header_only(9000, 9000, 3)),
            None
        );
        // JPEG: the sides, from the frame; its bytes are not decoded, so no product limit
        let big = jpeg(0xc0, 8, 8192, 8192, 3);
        assert!(decode("image/jpeg", &big).is_some());
        assert_eq!(over_budget("image/jpeg", &big), None);
        let over = jpeg(0xc2, 8, 8193, 10, 3);
        assert_eq!(decode("image/jpeg", &over), None);
        assert_eq!(over_budget("image/jpeg", &over), Some((8193, 10)));
        assert_eq!(
            over_budget("image/jpeg", &jpeg(0xc0, 8, 10, 9000, 1)),
            Some((10, 9000))
        );
        assert_eq!(over_budget("image/gif", &over), None);
    }

    #[test]
    fn ec9_png_inflate_is_given_the_header_limit() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 % 251) as u8).collect();
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&data, 6);
        assert_eq!(inflate(&z, 1000), Some(data.clone()));
        // more output than the limit, or less, does not inflate
        assert_eq!(inflate(&z, 999), None);
        assert_eq!(inflate(&z, 1001), None);
        // a small stream that inflates to 64 MiB stops at the limit
        let zeros = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; 64 << 20], 6);
        assert!(zeros.len() < 1 << 20);
        assert_eq!(inflate(&zeros, 4096), None);
        // and a PNG whose data inflates past its header does not decode
        let rows = pixels(2, 3, 3);
        assert_eq!(
            decode("image/png", &png(2, 2, 8, 2, 0, &filter(&rows, 3))),
            None
        );
    }
}
