//! dgm2 (D-FY4VF, D-P687F; n-62GWK): a build reads each figure blob's state from its metadata and
//! reads bytes only for the blobs its selected output shows. A removed figure keeps the existing
//! notice; a named profile never fetches the bytes of a figure outside its selection.
// the shared fixture is also a module of `render_publication_site` (pub6), which keeps it private
#[allow(clippy::duplicate_mod)]
#[path = "../../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::{BlobState, Rdy, Scope, publication};
use rdy_model::redaction::RedactTarget;
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::golden::tiny;
use rdy_render::{Mode, build_profile_site, build_site};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// The notice a page writes in place of removed content.
const NOTICE_HTML: &str = "<p class=\"redacted\">Content removed.</p>";

fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, BlockId> {
    let w = write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e}"));
    w.created
        .iter()
        .map(|(k, v)| (k.clone(), BlockId::parse(v).unwrap()))
        .collect()
}

/// The paths of `files` whose bytes hold `bytes`.
fn carries(files: &BTreeMap<String, Vec<u8>>, bytes: &[u8]) -> Vec<String> {
    files
        .iter()
        .filter(|(_, b)| b.windows(bytes.len()).any(|w| w == bytes))
        .map(|(p, _)| p.clone())
        .collect()
}

fn page(files: &BTreeMap<String, Vec<u8>>, path: &str) -> String {
    String::from_utf8(
        files
            .get(path)
            .unwrap_or_else(|| panic!("no file {path}: {:?}", files.keys()))
            .clone(),
    )
    .unwrap()
}

/// Applies one approved removal of `targets` to the closed project at `dir`, then opens it.
fn redact(dir: &Path, targets: Vec<RedactTarget>) -> (Rdy, Hash) {
    let f = authorized_batch_fixture(
        &dir.join(".rdy/store"),
        &[RequestSpec::new("redact", targets)],
        &FixtureOptions::approved(Proof::relayed_human()),
    );
    (Rdy::open(dir).unwrap(), f.requests[0].request)
}

/// A generic picture's bytes: a PNG signature and a distinct tail.
fn picture(tail: &str) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    b.extend_from_slice(tail.as_bytes());
    b
}

#[test]
fn dgm2_profile_and_redacted_figures_never_read_hidden_bytes() {
    let (dir, mut r) = tiny();
    let shown_png = picture("shown figure bytes");
    let hidden_png = picture("excluded figure bytes");
    let removed_png = picture("removed figure bytes");
    let shown = r.store_mut().put_blob(&shown_png).unwrap();
    let hidden = r.store_mut().put_blob(&hidden_png).unwrap();
    let removed = r.store_mut().put_blob(&removed_png).unwrap();
    let fig = |id: &str, parent: &str, blob: &Hash| {
        json!({"op": "create", "type": "figure", "id": id, "parent": parent,
            "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": "A figure"}})
    };
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$plan", "fields": {"name": "plan", "title": "Plan page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$plan", "body": "Plan words."},
            fig("$fs", "$plan", &shown),
            fig("$fr", "$plan", &removed),
            {"op": "create", "type": "doc", "id": "$other", "fields": {"name": "other", "title": "Other page", "doc_type": "page"}},
            fig("$fh", "$other", &hidden),
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    put(
        &mut r,
        "w1",
        json!([{"op": "create", "type": "publication_profile", "parent": ids["$admin"].to_string(),
            "fields": {"key": "guide", "title": "Guide", "audience": "org",
                "select": [ids["$plan"].to_string()]}}]),
    );
    land(&mut r, "w1").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    // a landing since the tag and an open view: the build reads the tag's and the base's sides
    put(
        &mut r,
        "w2",
        json!([{"op": "update", "id": ids["$fh"].to_string(), "fields": {"alt": "Edited"}}]),
    );
    land(&mut r, "w2").unwrap();
    put(
        &mut r,
        "work",
        json!([{"op": "create", "type": "prose", "parent": ids["$plan"].to_string(), "body": "Under review."}]),
    );
    r.close().unwrap();
    let (r, request) = redact(dir.path(), vec![RedactTarget::Blob(removed)]);
    let st = r.read();

    // the metadata states
    assert_eq!(st.reader().blob_state(&shown).unwrap(), BlobState::Readable);
    assert_eq!(
        st.reader().blob_state(&hidden).unwrap(),
        BlobState::Readable
    );
    assert_eq!(
        st.reader().blob_state(&removed).unwrap(),
        BlobState::Removed(request)
    );

    // the profile's output: the selected figure's bytes, the removed one's notice, and only the
    // selected readable blob fetched
    for scope in ["canon", "tag:v1.0.0"] {
        let before = publication::blob_reads_for_tests();
        let s = build_profile_site(&st, scope, "guide").unwrap();
        assert_eq!(publication::blob_reads_for_tests() - before, 1, "{scope}");
        assert!(!carries(&s.files, &shown_png).is_empty(), "{scope}");
        assert_eq!(
            carries(&s.files, &hidden_png),
            Vec::<String>::new(),
            "{scope}"
        );
        assert_eq!(
            carries(&s.files, &removed_png),
            Vec::<String>::new(),
            "{scope}"
        );
        let plan = page(&s.files, "plan.html");
        assert_eq!(plan.matches(NOTICE_HTML).count(), 1, "{scope}: {plan}");
    }

    // the source outputs: a canon site and a review build across the tag's history
    for scope in [Scope::Canon, Scope::View("work".into())] {
        let s = build_site(&st, &scope, Mode::Static).unwrap();
        assert!(!carries(&s.files, &shown_png).is_empty(), "{scope:?}");
        assert!(!carries(&s.files, &hidden_png).is_empty(), "{scope:?}");
        assert_eq!(
            carries(&s.files, &removed_png),
            Vec::<String>::new(),
            "{scope:?}"
        );
        let plan = page(&s.files, "plan.html");
        assert_eq!(plan.matches(NOTICE_HTML).count(), 1, "{scope:?}: {plan}");
    }
}
