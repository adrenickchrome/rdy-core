//! hpb1 (D-KDFZ3, hub n-CT2GB): the publication manifest's recipe 1, its validation, and the
//! incremental artifact hash. The expected hashes were calculated with an independent BLAKE3
//! implementation (a transcription of the specification's reference code), not with this crate.
use rdy_engine::publication_manifest::{
    ArtifactHasher, MANIFEST_RECIPE, Manifest, ManifestEntry, NOTICE_SLOT, hash_reader,
    manifest_hash, validate_manifest,
};
use rdy_engine::{RdyError, did_you_mean};
use rdy_model::Hash;
use std::io::{self, Read};

const GIB2: u64 = 2_147_483_648;

fn entry(path: &str, hash: [u8; 32], media: &str, size: u64, offset: Option<u64>) -> ManifestEntry {
    ManifestEntry {
        path: path.into(),
        hash: Hash(hash),
        media_type: media.into(),
        size,
        notice_offset: offset,
    }
}

fn manifest(files: Vec<ManifestEntry>) -> Manifest {
    Manifest { files }
}

fn hex(s: &str) -> Hash {
    s.parse().unwrap()
}

fn counting(n: u8) -> [u8; 32] {
    std::array::from_fn(|i| n.wrapping_add(i as u8))
}

#[track_caller]
fn assert_err(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix)
    );
}

#[track_caller]
fn invalid(m: &Manifest) {
    let e = manifest_hash(m, MANIFEST_RECIPE).unwrap_err();
    assert_err(
        &e,
        "E_BAD_INPUT",
        "publication manifest is invalid",
        "use unique relative paths and valid file metadata",
    );
    assert_eq!(validate_manifest(m).unwrap_err().message, e.message);
}

#[track_caller]
fn over(m: &Manifest, limit: &str) {
    let e = manifest_hash(m, MANIFEST_RECIPE).unwrap_err();
    assert_err(
        &e,
        "E_LIMIT",
        &format!("publication manifest exceeds {limit}"),
        "reduce the artifact to the stated limit",
    );
}

#[track_caller]
fn fine(m: &Manifest) -> Hash {
    validate_manifest(m).unwrap();
    manifest_hash(m, MANIFEST_RECIPE).unwrap()
}

fn one(path: &str) -> Manifest {
    manifest(vec![entry(path, [0; 32], "text/plain", 1, None)])
}

fn media(media_type: &str) -> Manifest {
    manifest(vec![entry("a", [0; 32], media_type, 1, None)])
}

/// Vector A: two files given out of order; one HTML file with a notice slot.
fn vector_a() -> Manifest {
    manifest(vec![
        entry("index.html", [0x11; 32], "text/html", 1234, Some(100)),
        entry("assets/site.css", counting(0), "text/css", 0, None),
    ])
}

/// Vector B: three files whose sizes add up to the artifact limit exactly; the slot at offset 0.
fn vector_b() -> Manifest {
    manifest(vec![
        entry("z.html", [0xa5; 32], "text/html", 18, Some(0)),
        entry("a", [0; 32], "application/octet-stream", GIB2 - 36, None),
        entry("A-b/c_d.E9", [0xff; 32], "image/svg+xml", 18, None),
    ])
}

fn u32le(n: u32) -> Vec<u8> {
    n.to_le_bytes().to_vec()
}

fn u64le(n: u64) -> Vec<u8> {
    n.to_le_bytes().to_vec()
}

#[test]
fn hpb1_manifest_recipe_has_fixed_vectors() {
    assert_eq!(MANIFEST_RECIPE, 1);
    assert_eq!(NOTICE_SLOT, b"<!--rdy-notices-->");
    assert_eq!(NOTICE_SLOT.len(), 18);

    let head = |count: u32| -> Vec<u8> {
        [
            b"rdy-publication-manifest\0".to_vec(),
            u32le(1),
            u32le(count),
        ]
        .concat()
    };
    let pre_a: Vec<u8> = [
        head(2),
        // assets/site.css sorts first
        u32le(15),
        b"assets/site.css".to_vec(),
        counting(0).to_vec(),
        u32le(8),
        b"text/css".to_vec(),
        u64le(0),
        vec![0],
        // index.html
        u32le(10),
        b"index.html".to_vec(),
        vec![0x11; 32],
        u32le(9),
        b"text/html".to_vec(),
        u64le(1234),
        vec![1],
        u64le(100),
    ]
    .concat();
    assert_eq!(pre_a.len(), 181);
    let want_a = hex("fec7a84cd0b0a6e97d151a2e8ad53c8adac9a0ee27144fb855437034f2a98971");
    assert_eq!(Hash::of(&pre_a), want_a, "the preimage's BLAKE3");
    assert_eq!(fine(&vector_a()), want_a);

    let pre_b: Vec<u8> = [
        head(3),
        // byte order: "A-b/c_d.E9" < "a" < "z.html"
        u32le(10),
        b"A-b/c_d.E9".to_vec(),
        vec![0xff; 32],
        u32le(13),
        b"image/svg+xml".to_vec(),
        u64le(18),
        vec![0],
        u32le(1),
        b"a".to_vec(),
        vec![0; 32],
        u32le(24),
        b"application/octet-stream".to_vec(),
        u64le(GIB2 - 36),
        vec![0],
        u32le(6),
        b"z.html".to_vec(),
        vec![0xa5; 32],
        u32le(9),
        b"text/html".to_vec(),
        u64le(18),
        vec![1],
        u64le(0),
    ]
    .concat();
    assert_eq!(pre_b.len(), 251);
    let want_b = hex("90bfec9079e2d2b96c3dd68402590b774c35c3321892af901d03716c39433ce4");
    assert_eq!(Hash::of(&pre_b), want_b, "the preimage's BLAKE3");
    assert_eq!(fine(&vector_b()), want_b);

    // other recipes are refused, whatever the manifest
    for recipe in [0, 2, u32::MAX] {
        let e = manifest_hash(&vector_a(), recipe).unwrap_err();
        assert_err(
            &e,
            "E_BAD_INPUT",
            &format!("manifest recipe {recipe} is not supported"),
            "use manifest recipe 1",
        );
    }
}

#[test]
fn hpb1_manifest_order_is_canonical() {
    for m in [vector_a(), vector_b()] {
        let mut reversed = m.clone();
        reversed.files.reverse();
        assert_eq!(fine(&m), fine(&reversed));
        let mut rotated = m.clone();
        rotated.files.rotate_left(1);
        assert_eq!(fine(&m), fine(&rotated));
    }
    // UTF-8 byte order, not case-folded order: "B" sorts before "a"
    let ab = manifest(vec![
        entry("a", [1; 32], "text/plain", 1, None),
        entry("B", [2; 32], "text/plain", 1, None),
    ]);
    let mut ba = ab.clone();
    ba.files.reverse();
    assert_eq!(fine(&ab), fine(&ba));
}

#[test]
fn hpb1_every_manifest_field_changes_the_hash() {
    let base = || entry("p/index.html", [7; 32], "text/html", 100, None);
    let mut seen = std::collections::BTreeSet::new();
    let mut add = |e: ManifestEntry, what: &str| {
        let m = manifest(vec![e, entry("other", [9; 32], "text/plain", 3, None)]);
        assert!(seen.insert(fine(&m)), "{what} changes the hash");
    };
    add(base(), "base");
    add(
        ManifestEntry {
            path: "p/index2.html".into(),
            ..base()
        },
        "path",
    );
    let mut h = [7; 32];
    h[31] = 8;
    add(
        ManifestEntry {
            hash: Hash(h),
            ..base()
        },
        "hash",
    );
    add(
        ManifestEntry {
            media_type: "text/htm".into(),
            ..base()
        },
        "media type",
    );
    add(
        ManifestEntry {
            size: 101,
            ..base()
        },
        "size",
    );
    add(
        ManifestEntry {
            notice_offset: Some(0),
            ..base()
        },
        "a zero offset against no offset",
    );
    add(
        ManifestEntry {
            notice_offset: Some(1),
            ..base()
        },
        "offset",
    );
    // the file count too
    let m = manifest(vec![base()]);
    assert!(seen.insert(fine(&m)), "file count");
}

#[test]
fn hpb1_manifest_refuses_collisions_paths_and_bounds() {
    // file count: 1 to 32,000
    invalid(&manifest(vec![]));
    let many = |n: usize| {
        manifest(
            (0..n)
                .map(|i| entry(&format!("f/{i}"), [0; 32], "text/plain", 1, None))
                .collect(),
        )
    };
    fine(&many(32_000));
    over(&many(32_001), "32000 files");

    // path bytes: 17 segments of 240 and 16 slashes are 4096 bytes
    let seg = "s".repeat(240);
    let at_limit = vec![seg.as_str(); 17].join("/");
    assert_eq!(at_limit.len(), 4096);
    fine(&one(&at_limit));
    over(&one(&format!("{at_limit}x")), "4096 path bytes");

    // segment bytes
    fine(&one(&format!("d/{}", "x".repeat(255))));
    over(&one(&format!("d/{}", "x".repeat(256))), "255 segment bytes");

    // path form
    fine(&one("A-z_0.9/b.c"));
    for bad in [
        "", "/a", "a/", "a//b", ".", "..", "a/.", "./a", "a/../b", "a\\b", "a%20b", "a:b",
        "a\u{0}b", "a\u{1f}b", "a\u{7f}b", "a b", "a~b", "é", "a+b", "a?b", "a#b",
    ] {
        invalid(&one(bad));
    }
    // two dots inside a segment are a name, not a parent
    fine(&one("a..b/.c/d."));

    // uniqueness, case-folded
    let two = |a: &str, b: &str| {
        manifest(vec![
            entry(a, [0; 32], "text/plain", 1, None),
            entry(b, [1; 32], "text/plain", 1, None),
        ])
    };
    invalid(&two("index.html", "index.html"));
    invalid(&two("Index.html", "index.html"));
    invalid(&two("docs/A", "DOCS/a"));
    fine(&two("docs/a", "docs/a-"));

    // media type
    let m128 = format!("a/{}", "b".repeat(126));
    assert_eq!(m128.len(), 128);
    fine(&media(&m128));
    over(&media(&format!("{m128}c")), "128 media-type bytes");
    for ok in [
        "text/html",
        "image/svg+xml",
        "application/vnd.rdy-1.0+json",
        "x/y",
    ] {
        fine(&media(ok));
    }
    for bad in [
        "",
        "text",
        "text/",
        "/html",
        "text/html/x",
        "Text/html",
        "text/HTML",
        "text/html; charset=utf-8",
        "text/html;charset=utf-8",
        "text /html",
        "text/ht_ml",
        "text/*",
    ] {
        invalid(&media(bad));
    }

    // sizes: a file and the artifact at most 2 GiB
    fine(&manifest(vec![entry(
        "a",
        [0; 32],
        "text/plain",
        GIB2,
        None,
    )]));
    over(
        &manifest(vec![entry("a", [0; 32], "text/plain", GIB2 + 1, None)]),
        "2147483648 file bytes",
    );
    over(
        &manifest(vec![entry("a", [0; 32], "text/plain", u64::MAX, None)]),
        "2147483648 file bytes",
    );
    let pair = |a: u64, b: u64| {
        manifest(vec![
            entry("a", [0; 32], "text/plain", a, None),
            entry("b", [0; 32], "text/plain", b, None),
        ])
    };
    fine(&pair(GIB2 - 1, 1));
    over(&pair(GIB2, 1), "2147483648 artifact bytes");
    over(&pair(GIB2, GIB2), "2147483648 artifact bytes");

    // notice offset: text/html only, with room for the 18-byte slot
    let html = |size: u64, offset: u64| {
        manifest(vec![entry(
            "i.html",
            [0; 32],
            "text/html",
            size,
            Some(offset),
        )])
    };
    fine(&html(18, 0));
    fine(&html(100, 82));
    invalid(&html(100, 83));
    invalid(&html(17, 0));
    invalid(&html(GIB2, u64::MAX));
    invalid(&html(GIB2, u64::MAX - 17));
    invalid(&manifest(vec![entry(
        "i.css",
        [0; 32],
        "text/css",
        100,
        Some(0),
    )]));
    invalid(&manifest(vec![entry(
        "i.xhtml",
        [0; 32],
        "application/xhtml+xml",
        100,
        Some(0),
    )]));
}

#[test]
fn hpb1_manifest_json_is_strict_with_hex_hashes() {
    let m = vector_a();
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(
        v["files"][0]["hash"],
        "1111111111111111111111111111111111111111111111111111111111111111"
    );
    assert_eq!(v["files"][1]["notice_offset"], serde_json::Value::Null);
    let back: Manifest = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(back, m);

    let mut upper = v.clone();
    upper["files"][0]["hash"] = format!("{}ABC", "1".repeat(61)).into();
    assert!(serde_json::from_value::<Manifest>(upper).is_err());
    let mut short = v.clone();
    short["files"][0]["hash"] = "11".into();
    assert!(serde_json::from_value::<Manifest>(short).is_err());
    let mut array = v.clone();
    array["files"][0]["hash"] = serde_json::to_value([0x11u8; 32]).unwrap();
    assert!(serde_json::from_value::<Manifest>(array).is_err());

    for (at, key) in [("top", "recipe"), ("entry", "tag")] {
        let mut extra = v.clone();
        match at {
            "top" => extra[key] = 1.into(),
            _ => extra["files"][0][key] = 1.into(),
        }
        let msg = serde_json::from_value::<Manifest>(extra)
            .unwrap_err()
            .to_string();
        let e = did_you_mean::unknown_field_error("publication manifest", &msg)
            .expect("an unknown-field error");
        assert_eq!(e.code, "E_BAD_INPUT");
        assert!(
            e.message.ends_with(&format!("unknown field \"{key}\"")),
            "{e}"
        );
    }
}

/// The official BLAKE3 test input: byte i is i mod 251.
fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

fn incremental(bytes: &[u8], chunk: usize) -> Hash {
    let mut h = ArtifactHasher::new(bytes.len() as u64).unwrap();
    for c in bytes.chunks(chunk.max(1)) {
        h.update(c).unwrap();
    }
    h.finish().unwrap()
}

/// A reader that hands out at most `step` bytes a call.
struct Trickle<'a> {
    bytes: &'a [u8],
    step: usize,
}

impl Read for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.step.min(buf.len()).min(self.bytes.len());
        buf[..n].copy_from_slice(&self.bytes[..n]);
        self.bytes = &self.bytes[n..];
        Ok(n)
    }
}

/// A reader that fails after `ok` bytes, once interrupted first.
struct Failing {
    ok: usize,
    interrupted: bool,
}

impl Read for Failing {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.interrupted {
            self.interrupted = true;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        if self.ok == 0 {
            return Err(io::Error::other("disk gone"));
        }
        let n = self.ok.min(buf.len());
        buf[..n].fill(0);
        self.ok -= n;
        Ok(n)
    }
}

#[track_caller]
fn mismatch(e: &RdyError) {
    assert_err(
        e,
        "E_BAD_INPUT",
        "publication artifact does not match its manifest",
        "render the publication again before uploading",
    );
}

#[test]
fn hpb1_incremental_hash_matches_whole_bytes() {
    // the official vectors, by input length
    let known = [
        (
            0,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
        ),
        (
            1,
            "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213",
        ),
        (
            1023,
            "10108970eeda3eb932baac1428c7a2163b0e924c9a9e25b35bba72b28f70bd11",
        ),
        (
            1024,
            "42214739f095a406f3fc83deb889744ac00df831c10daa55189b5d121c855af7",
        ),
        (
            1025,
            "d00278ae47eb27b34faecf67b4fe263f82d5412916c1ffd97c8cb7fb814b8444",
        ),
        (
            2048,
            "e776b6028c7cd22a4d0ba182a8bf62205d2ef576467e838ed6f2529b85fba24a",
        ),
        (
            2049,
            "5f4d72f40d7a5f82b15ca2b2e44b1de3c2ef86c426c95c1af0b6879522563030",
        ),
        (
            3072,
            "b98cb0ff3623be03326b373de6b9095218513e64f1ee2edd2525c7ad1e5cffd2",
        ),
        (
            8192,
            "aae792484c8efe4f19e2ca7d371d8c467ffb10748d8a5a1ae579948f718a2a63",
        ),
        (
            31744,
            "62b6960e1a44bcc1eb1a611a8d6235b6b4b78f32e7abc4fb4c6cdcce94895c47",
        ),
    ];
    for (n, want) in known {
        let bytes = pattern(n);
        let want = hex(want);
        assert_eq!(Hash::of(&bytes), want, "Hash::of over {n} bytes");
        for chunk in [1, 63, 64, 65, 1023, 1024, 1025, 65_536] {
            assert_eq!(incremental(&bytes, chunk), want, "{n} bytes in {chunk}s");
        }
        // empty updates between chunks change nothing
        let mut h = ArtifactHasher::new(n as u64).unwrap();
        for c in bytes.chunks(700) {
            h.update(&[]).unwrap();
            h.update(c).unwrap();
        }
        assert_eq!(h.finish().unwrap(), want);
        // a reader, whole and trickled
        assert_eq!(hash_reader(&mut &bytes[..], n as u64).unwrap(), want);
        let mut t = Trickle {
            bytes: &bytes,
            step: 777,
        };
        assert_eq!(hash_reader(&mut t, n as u64).unwrap(), want);
    }
    assert_eq!(
        Hash::of(b"abc"),
        hex("6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85")
    );

    // a larger input across the 65,536-byte read buffer
    let big = pattern(200_003);
    assert_eq!(hash_reader(&mut &big[..], 200_003).unwrap(), Hash::of(&big));
    assert_eq!(incremental(&big, 65_536), Hash::of(&big));

    // short and long input
    let ten = pattern(10);
    let mut h = ArtifactHasher::new(11).unwrap();
    h.update(&ten).unwrap();
    mismatch(&h.finish().unwrap_err());
    let mut h = ArtifactHasher::new(9).unwrap();
    mismatch(&h.update(&ten).unwrap_err());
    let mut h = ArtifactHasher::new(10).unwrap();
    h.update(&ten[..5]).unwrap();
    mismatch(&h.update(&ten[..6]).unwrap_err());
    // a refused update leaves the hasher refused
    mismatch(&h.finish().unwrap_err());
    mismatch(&hash_reader(&mut &ten[..], 9).unwrap_err());
    mismatch(&hash_reader(&mut &ten[..], 11).unwrap_err());
    mismatch(&hash_reader(&mut &ten[..], 0).unwrap_err());
    let mut h = ArtifactHasher::new(0).unwrap();
    mismatch(&h.update(b"x").unwrap_err());

    // the bound: 2 GiB
    ArtifactHasher::new(GIB2).unwrap();
    for n in [GIB2 + 1, u64::MAX] {
        let e = ArtifactHasher::new(n).err().unwrap();
        assert_err(
            &e,
            "E_LIMIT",
            "publication manifest exceeds 2147483648 file bytes",
            "reduce the artifact to the stated limit",
        );
        assert_eq!(hash_reader(&mut &ten[..], n).unwrap_err().code, "E_LIMIT");
    }

    // a read error, after an interruption that is retried
    let e = hash_reader(
        &mut Failing {
            ok: 4,
            interrupted: false,
        },
        10,
    )
    .unwrap_err();
    assert_err(
        &e,
        "E_IO",
        "publication artifact could not be read",
        "render the publication again before uploading",
    );
    assert_eq!(
        hash_reader(
            &mut Failing {
                ok: 0,
                interrupted: true
            },
            0
        )
        .unwrap_err()
        .code,
        "E_IO",
        "an empty artifact still reads to its end"
    );
}
