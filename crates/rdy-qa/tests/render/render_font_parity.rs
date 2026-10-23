//! plx1 (D-WQSAY, D-EVRNM): the PDF writer's font reader can change, its output cannot. The
//! golden project's canon, `v1.0.0` tag and `editing` view print to the same PDF bytes as the
//! base reader's (the producer version masked, so a version bump keeps the pin).
use rdy_model::Hash;
use rdy_qa::golden::project;
use rdy_render::print::{self, fonts::Fonts};

/// The PDF with its `/Producer (rdy <version>)` text masked to the same length, so a version
/// bump keeps the digest while the version keeps its length (as `render_publication_prepare`).
fn versionless(pdf: &[u8]) -> Vec<u8> {
    let producer = format!("rdy {}", env!("CARGO_PKG_VERSION")).into_bytes();
    let mask = [b"rdy ".as_slice(), &vec![b'#'; producer.len() - 4]].concat();
    let mut out = pdf.to_vec();
    let at = out
        .windows(producer.len())
        .position(|w| w == producer.as_slice())
        .expect("the producer text in the PDF");
    out[at..at + producer.len()].copy_from_slice(&mask);
    out
}

/// Returns this thread's ids to random ones on every path out of the test.
struct Unseed;

impl Drop for Unseed {
    fn drop(&mut self) {
        rdy_model::unseed_ids_for_tests();
    }
}

/// The PDF digests (producer version masked) of the base font reader, captured before it was
/// replaced: (scope, digest).
const BASE_PDF_DIGESTS: [(&str, &str); 3] = [
    (
        "canon",
        "2dd5e4650ee46a48f413a5ee0dc3e49000fc15ea56a8d8567fcea057281dae0c",
    ),
    (
        "tag:v1.0.0",
        "da18223eb0fe39879a9335339d894a4889da5a520b22f232e7bc2568ae347403",
    ),
    (
        "view:editing",
        "c7552ff804b45edf2d78b60e165522ae44440a7e8a27bc98d3071cc19d0bfcf7",
    ),
];

#[test]
fn plx1_print_pdf_and_site_match_base_bytes() {
    let _unseed = Unseed;
    let fonts = Fonts::load();
    let mut got = vec![];
    for (scope, _) in BASE_PDF_DIGESTS {
        rdy_model::seed_ids_for_tests(7);
        let (_dir, r, _ids) = project();
        let st = r.read();
        let s = st.resolve_scope(scope).expect("the scope");
        let p = print::build(&st, &s).expect("the print");
        let (pdf, _, _) = print::pdf::pdf_bytes(&p, &fonts);
        got.push((scope, Hash::of(&versionless(&pdf)).to_string()));
    }
    let got: Vec<(&str, &str)> = got.iter().map(|(s, d)| (*s, d.as_str())).collect();
    assert_eq!(got, BASE_PDF_DIGESTS.to_vec(), "{got:#?}");
}
