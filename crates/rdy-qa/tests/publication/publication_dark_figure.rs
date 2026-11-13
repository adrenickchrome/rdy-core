//! rfr2 (D-TBDA1, D-3DAX4): a paired figure gives two attachment rows. The light row keeps its
//! fields and has no `variant`; the dark row adds `variant: "dark"`. Rows sort by home doc,
//! block, then variant, light first; a comparison pairs rows by block and variant; `available`
//! describes that row's blob; both rows count toward the limits and the report's rows; the
//! selection identity stays the selected ids and versions. A remote image's URL is not a
//! reference.
use rdy_engine::publication::{self, AttachmentVariant, REPORT_ROWS};
use rdy_engine::{Rdy, Scope};
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::{approver, land, write};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn put(r: &mut Rdy, ops: Value) -> BTreeMap<String, BlockId> {
    let w = write(r, "w", ops).unwrap_or_else(|e| panic!("write: {e}"));
    w.created
        .iter()
        .map(|(k, v)| (k.clone(), BlockId::parse(v).unwrap()))
        .collect()
}

fn picture(tail: &str) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    b.extend_from_slice(tail.as_bytes());
    b
}

fn row(
    home: &BlockId,
    block: &BlockId,
    variant: Option<AttachmentVariant>,
    blob: Hash,
    available: bool,
) -> (BlockId, BlockId, Option<AttachmentVariant>, Hash, bool) {
    (home.clone(), block.clone(), variant, blob, available)
}

fn rows(
    res: &publication::Resolved,
) -> Vec<(BlockId, BlockId, Option<AttachmentVariant>, Hash, bool)> {
    res.attachments
        .iter()
        .map(|a| row(&a.home_doc, &a.block, a.variant, a.blob, a.available))
        .collect()
}

/// The first value of `key` anywhere in `v`.
fn find<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m.get(key).or_else(|| m.values().find_map(|x| find(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find(x, key)),
        _ => None,
    }
}

const DARK: Option<AttachmentVariant> = Some(AttachmentVariant::Dark);

struct World {
    _dir: tempfile::TempDir,
    r: Rdy,
    plan: BlockId,
    fa: BlockId,
    fb: BlockId,
    blobs: BTreeMap<&'static str, Hash>,
}

/// A doc `plan` with a paired figure `fa` (light `la`, dark `da`) and a light-only figure `fb`
/// (light `lb`), selected by the profile `guide`; landed and tagged `v1.0.0`.
fn world() -> World {
    let (dir, mut r) = rdy_qa::golden::tiny();
    let mut blobs = BTreeMap::new();
    for name in ["la", "da", "lb", "db", "da2", "la2"] {
        blobs.insert(name, r.store_mut().put_blob(&picture(name)).unwrap());
    }
    let fig = |id: &str, light: &Hash, dark: Option<&Hash>| {
        let mut fields =
            json!({"blob": light.to_string(), "media_type": "image/png", "alt": "A figure"});
        if let Some(d) = dark {
            fields["dark_blob"] = json!(d.to_string());
            fields["dark_media_type"] = json!("image/png");
        }
        json!({"op": "create", "type": "figure", "id": id, "parent": "$plan", "fields": fields})
    };
    let ids = put(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$plan", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            fig("$fa", &blobs["la"], Some(&blobs["da"])),
            fig("$fb", &blobs["lb"], None),
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    put(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "parent": ids["$admin"].to_string(),
            "fields": {"key": "guide", "title": "Guide", "audience": "org",
                "select": [ids["$plan"].to_string()]}}]),
    );
    land(&mut r, "w").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    World {
        _dir: dir,
        r,
        plan: ids["$plan"].clone(),
        fa: ids["$fa"].clone(),
        fb: ids["$fb"].clone(),
        blobs,
    }
}

#[test]
fn rfr2_publication_tracks_both_variants() {
    let mut w = world();
    let b = w.blobs.clone();
    let st = w.r.read();
    let base = publication::resolve(&st, &Scope::Canon, "guide").unwrap();
    // exact rows, sorted by home doc, block, then variant (light first)
    let mut want = vec![
        row(&w.plan, &w.fa, None, b["la"], true),
        row(&w.plan, &w.fa, DARK, b["da"], true),
        row(&w.plan, &w.fb, None, b["lb"], true),
    ];
    want.sort_by(|x, y| (&x.0, &x.1, x.2).cmp(&(&y.0, &y.1, y.2)));
    assert_eq!(rows(&base), want);
    let light_at = base
        .attachments
        .iter()
        .position(|a| a.block == w.fa)
        .unwrap();
    assert_eq!(base.attachments[light_at].variant, None, "light first");
    assert_eq!(base.attachments[light_at + 1].variant, DARK);
    assert_eq!(AttachmentVariant::Dark.as_str(), "dark");

    // per-row availability, the selection identity and the limits: rdy-engine's
    // `rfr2_both_rows_count_toward_the_limits_and_availability` (test-only hooks)
    drop(st);

    // a variant changes, is added and is removed: each a row of its own, keyed by block and
    // variant; a light change keeps no variant
    let fa = w.fa.to_string();
    let fb = w.fb.to_string();
    put(
        &mut w.r,
        json!([
            {"op": "update", "id": fa, "fields": {"blob": b["la2"].to_string(), "dark_blob": b["da2"].to_string()}},
            {"op": "update", "id": fb, "fields": {"dark_blob": b["db"].to_string(), "dark_media_type": "image/png"}}
        ]),
    );
    land(&mut w.r, "w").unwrap();
    w.r.tag(&approver(), "v2.0.0", None, None).unwrap();
    let v1 = publication::resolve(
        &w.r.read(),
        &Scope::At(tag_landing(&w.r, "v1.0.0")),
        "guide",
    )
    .unwrap();
    let v2 = publication::resolve(
        &w.r.read(),
        &Scope::At(tag_landing(&w.r, "v2.0.0")),
        "guide",
    )
    .unwrap();
    let d = publication::disclosure_diff(&v1, &v2).unwrap();
    assert_eq!(
        d.added_attachments
            .iter()
            .map(|a| (a.block.clone(), a.variant, a.blob))
            .collect::<Vec<_>>(),
        vec![(w.fb.clone(), DARK, b["db"])]
    );
    assert!(d.removed_attachments.is_empty());
    let mut changed: Vec<_> = d
        .changed_attachments
        .iter()
        .map(|c| (c.block.clone(), c.variant, c.before_blob, c.after_blob))
        .collect();
    changed.sort();
    assert_eq!(
        changed,
        vec![
            (w.fa.clone(), None, b["la"], b["la2"]),
            (w.fa.clone(), DARK, b["da"], b["da2"])
        ]
    );
    // the report: a dark row says so; a light row has no variant field
    let report = publication::check(
        &w.r.read(),
        "guide",
        Some("tag:v2.0.0"),
        Some("tag:v1.0.0"),
        None,
    )
    .unwrap();
    let added = find(&report, "added_attachments").unwrap();
    assert_eq!(added[0]["variant"], json!("dark"), "{added}");
    let ch = find(&report, "changed_attachments")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(ch.len(), 2);
    for c in ch {
        match c.get("variant") {
            Some(v) => assert_eq!(v, "dark"),
            None => assert_eq!(c["after_blob"], json!(b["la2"].to_string())),
        }
    }
    put(
        &mut w.r,
        json!([{"op": "update", "id": fb, "unset": ["dark_blob", "dark_media_type"]}]),
    );
    land(&mut w.r, "w").unwrap();
    w.r.tag(&approver(), "v3.0.0", None, None).unwrap();
    let v3 = publication::resolve(
        &w.r.read(),
        &Scope::At(tag_landing(&w.r, "v3.0.0")),
        "guide",
    )
    .unwrap();
    let d = publication::disclosure_diff(&v2, &v3).unwrap();
    assert_eq!(
        d.removed_attachments
            .iter()
            .map(|a| (a.block.clone(), a.variant, a.blob))
            .collect::<Vec<_>>(),
        vec![(w.fb.clone(), DARK, b["db"])]
    );
    assert!(d.added_attachments.is_empty() && d.changed_attachments.is_empty());
}

fn tag_landing(r: &Rdy, name: &str) -> Hash {
    let (_, tag, _) = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap();
    r.store().read_tag(&tag).unwrap().landing
}

/// The report's rows: both rows of a paired figure count toward its 200 rows.
#[test]
fn rfr2_paired_rows_count_toward_the_report_rows() {
    let (_dir, mut r) = rdy_qa::golden::tiny();
    let n = REPORT_ROWS / 2 + 1;
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$plan", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}}),
        json!({"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}),
        json!({"op": "create", "type": "publication_profile", "parent": "$admin",
            "fields": {"key": "guide", "title": "Guide", "audience": "org", "select": ["$plan"]}}),
    ];
    let mut figs = vec![];
    for i in 0..n {
        let l = r.store_mut().put_blob(&picture(&format!("l{i}"))).unwrap();
        let d = r.store_mut().put_blob(&picture(&format!("d{i}"))).unwrap();
        figs.push(
            json!({"op": "create", "type": "figure", "parent": "$plan", "fields": {
            "blob": l.to_string(), "media_type": "image/png",
            "dark_blob": d.to_string(), "dark_media_type": "image/png", "alt": "F"}}),
        );
    }
    // the profile's select names the doc by its id once it exists
    let ids = put(&mut r, Value::Array(ops.drain(..2).collect()));
    let mut profile = ops.remove(0);
    profile["parent"] = json!(ids["$admin"].to_string());
    profile["fields"]["select"] = json!([ids["$plan"].to_string()]);
    put(&mut r, json!([profile]));
    land(&mut r, "w").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    for f in &mut figs {
        f["parent"] = json!(ids["$plan"].to_string());
    }
    put(&mut r, Value::Array(figs));
    land(&mut r, "w").unwrap();
    let report = publication::check(&r.read(), "guide", None, Some("tag:v1.0.0"), None).unwrap();
    let added = find(&report, "added_attachments")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(added.len(), REPORT_ROWS);
    assert_eq!(REPORT_ROWS, 200);
    let omitted = find(&report, "omitted").unwrap();
    assert_eq!(
        omitted["added_attachments"],
        json!(2 * n - REPORT_ROWS),
        "{omitted}"
    );
    let dark_rows = added.iter().filter(|a| a.get("variant").is_some()).count();
    assert_eq!(dark_rows, REPORT_ROWS / 2);
}

/// A remote image's URL is not a block reference; a link around it keeps its own checks.
#[test]
fn rfr2_remote_image_urls_are_not_publication_refs() {
    let (_dir, mut r) = rdy_qa::golden::tiny();
    let ids = put(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$plan", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$other", "fields": {"name": "other", "title": "Other", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$t", "parent": "$other", "body": "Target words."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let t = ids["$t"].to_string();
    put(
        &mut r,
        json!([
            {"op": "create", "type": "prose", "parent": ids["$plan"].to_string(),
             "body": "Badge ![Build](https://img.example.com/badge.svg) and [![Docs](https://img.example.com/docs.svg)](https://docs.example.com/)."},
            {"op": "create", "type": "prose", "parent": ids["$plan"].to_string(),
             "body": format!("Linked [![Target](https://img.example.com/t.svg)]({{{{{t}}}}}).")},
            {"op": "create", "type": "publication_profile", "parent": ids["$admin"].to_string(),
             "fields": {"key": "guide", "title": "Guide", "audience": "org",
                "select": [ids["$plan"].to_string()]}}
        ]),
    );
    land(&mut r, "w").unwrap();
    let res = publication::resolve(&r.read(), &Scope::Canon, "guide").unwrap();
    // no image URL is a reference target
    for row in &res.references.rows {
        assert!(!row.target.contains("img.example.com"), "{row:?}");
    }
    // the link around an image to a block outside the selection keeps its check
    assert!(
        res.references
            .rows
            .iter()
            .any(|row| row.target.contains(&t)),
        "{:?}",
        res.references.rows
    );
}
