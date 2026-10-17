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

/// The image types that print (closed): JPEG (baseline or progressive; passed as DCTDecode) with 1
/// component (DeviceGray) or 3 (DeviceRGB) (O33), and
/// PNG of bit depth 8, colour type gray, RGB, gray with alpha or RGBA, not interlaced (decoded with
/// miniz_oxide; alpha becomes a soft mask). Anything else is None.
pub fn decode(media_type: &str, bytes: &[u8]) -> Option<Pixels> {
    match media_type {
        "image/jpeg" => jpeg(bytes),
        "image/png" => png(bytes),
        _ => None,
    }
}

/// A JPEG's size and components, from its frame header. Only a baseline (SOF0) or progressive
/// (SOF2) frame of 8-bit samples prints.
fn jpeg(bytes: &[u8]) -> Option<Pixels> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
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
            // the image ends, or its data starts, before a frame header
            0xd9 | 0xda => return None,
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes([*bytes.get(i)?, *bytes.get(i + 1)?]));
        if len < 2 {
            return None;
        }
        let seg = bytes.get(i + 2..i + len)?;
        match marker {
            0xc0 | 0xc2 => {
                let [precision, h0, h1, w0, w1, components, ..] = *seg else {
                    return None;
                };
                let (w, h) = (
                    u32::from(u16::from_be_bytes([w0, w1])),
                    u32::from(u16::from_be_bytes([h0, h1])),
                );
                if precision != 8 || w == 0 || h == 0 || !matches!(components, 1 | 3) {
                    return None;
                }
                return Some(Pixels::Jpeg {
                    w,
                    h,
                    components,
                    bytes: bytes.to_vec(),
                });
            }
            // every other frame type: extended, lossless, hierarchical or arithmetic-coded
            0xc1 | 0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => return None,
            _ => i += len,
        }
    }
}

/// A PNG's samples: bit depth 8, colour type 0 (gray), 2 (RGB), 4 (gray and alpha) or 6 (RGBA),
/// no interlace. The chunks' CRCs are not checked; the data must inflate to exactly the image.
fn png(bytes: &[u8]) -> Option<Pixels> {
    let mut rest = bytes.strip_prefix(b"\x89PNG\r\n\x1a\n")?;
    let mut header = None;
    let mut data = vec![];
    while rest.len() >= 12 {
        let len = usize::try_from(u32::from_be_bytes(rest[..4].try_into().ok()?)).ok()?;
        let kind: [u8; 4] = rest[4..8].try_into().ok()?;
        let body = rest.get(8..8usize.checked_add(len)?)?;
        match &kind {
            b"IHDR" => {
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
                ] = *body
                else {
                    return None;
                };
                header = Some((
                    u32::from_be_bytes([w0, w1, w2, w3]),
                    u32::from_be_bytes([h0, h1, h2, h3]),
                    depth,
                    color,
                    compression,
                    filter,
                    interlace,
                ));
            }
            b"IDAT" => data.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        rest = rest.get(12usize.checked_add(len)?..)?;
    }
    let (w, h, depth, color, compression, filter, interlace) = header?;
    let channels: usize = match color {
        0 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return None,
    };
    if depth != 8 || compression != 0 || filter != 0 || interlace != 0 || w == 0 || h == 0 {
        return None;
    }
    let (wu, hu) = (usize::try_from(w).ok()?, usize::try_from(h).ok()?);
    let stride = wu.checked_mul(channels)?;
    let size = stride.checked_add(1)?.checked_mul(hu)?;
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&data, size).ok()?;
    if raw.len() != size {
        return None;
    }
    let samples = unfilter(&raw, stride, channels)?;
    let (color, alpha) = match channels {
        1 | 3 => (samples, None),
        _ => {
            let c = channels - 1;
            let mut color = Vec::with_capacity(wu * hu * c);
            let mut alpha = Vec::with_capacity(wu * hu);
            for px in samples.chunks_exact(channels) {
                color.extend_from_slice(&px[..c]);
                alpha.push(px[c]);
            }
            (color, Some(alpha))
        }
    };
    Some(Pixels::Raw {
        w,
        h,
        gray: matches!(channels, 1 | 2),
        color,
        alpha,
    })
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
mod tests {
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
    fn png(w: u32, h: u32, depth: u8, color: u8, interlace: u8, filtered: &[u8]) -> Vec<u8> {
        png_with(w, h, depth, color, interlace, filtered, &[])
    }

    fn png_with(
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
    fn filter(rows: &[Vec<u8>], bpp: usize) -> Vec<u8> {
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
    fn pixels(w: usize, h: usize, bpp: usize) -> Vec<Vec<u8>> {
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

    /// A JPEG's markers up to its frame header, which is all [`decode`] reads.
    fn jpeg(sof: u8, precision: u8, w: u16, h: u16, components: u8) -> Vec<u8> {
        let mut out = vec![0xff, 0xd8];
        // an APP0 segment first
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
        out.extend_from_slice(&[0xff, 0xd9]);
        out
    }

    #[test]
    fn ec9_a_jpeg_passes_through_with_its_size() {
        for (sof, components) in [(0xc0, 3), (0xc2, 3), (0xc0, 1), (0xc2, 1)] {
            let bytes = jpeg(sof, 8, 640, 480, components);
            assert_eq!(
                decode("image/jpeg", &bytes),
                Some(Pixels::Jpeg {
                    w: 640,
                    h: 480,
                    components,
                    bytes: bytes.clone()
                }),
                "{sof:x} {components}"
            );
        }
        // CMYK, 12-bit, lossless and arithmetic-coded frames do not print (O33)
        assert_eq!(decode("image/jpeg", &jpeg(0xc0, 8, 4, 4, 4)), None);
        assert_eq!(decode("image/jpeg", &jpeg(0xc0, 12, 4, 4, 3)), None);
        assert_eq!(decode("image/jpeg", &jpeg(0xc3, 8, 4, 4, 3)), None);
        assert_eq!(decode("image/jpeg", &jpeg(0xc9, 8, 4, 4, 3)), None);
        assert_eq!(decode("image/jpeg", &jpeg(0xc0, 8, 0, 4, 3)), None);
        assert_eq!(decode("image/jpeg", b"\xff\xd8\xff"), None);
        assert_eq!(decode("image/png", &jpeg(0xc0, 8, 4, 4, 3)), None);
    }
}
