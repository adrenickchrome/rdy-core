//! The disclosure review fixture (drv6; D-RDH1C, D-9BWMV, D-0C4TB, D-MCH70): a project whose
//! publication check fills every list of the review. Ids are seeded and the clock is pinned, so
//! every id, version and landing is fixed and the review's goldens can pin its bytes.
//!
//! The org profile `review` hides planned blocks and selects the doc `intro` and the folder
//! `guides`. Tag `v1.0.0` holds the first state, tag `v2.0.0` the second, and canon a third with
//! three removals:
//!
//! - `intro` (changed): an updated prose with a code span, a decision that a new one supersedes,
//!   a second new decision,
//!   a new claim that links a decision at the root, a figure whose image changes, a removed
//!   figure, a reference outside the selection, a decision cited outside the selection, and a
//!   link edge outside it. One planned note is hidden at both tags.
//! - `setup` (added at `v2.0.0`, in `guides`): a section, prose with a code span, a decision, a
//!   figure with a dark image, a new reference outside the selection and a new planned note.
//! - `old` (in `guides` at `v1.0.0`, moved out of the selection at `v2.0.0`): a removed doc.
//! - `guides`: its title changes at `v2.0.0`.
//!
//! Every decision is built, so no decision makes the blocks beside it planned.
//! - canon removes the new claim, the second new decision and the doc `setup`. A removal
//!   removes every version of its block, so it shows at both tags too.
use crate::fixtures::{approver, land, project, write, writer};
use rdy_engine::pin_clock;
use rdy_engine::redaction::RedactInput;
use serde_json::{Value, json};

/// The profile the fixture checks.
pub const PROFILE: &str = "review";

/// The first tag.
pub const V1: &str = "tag:v1.0.0";

/// The second tag.
pub const V2: &str = "tag:v2.0.0";

/// The id seed of the fixture.
const SEED: u64 = 0x4452_5636;

/// The clock of the fixture.
const CLOCK_MS: i64 = 1_763_000_000_000;

fn png(r: &mut rdy_engine::Rdy, tail: &[u8]) -> String {
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    b.extend_from_slice(tail);
    r.store_mut().put_blob(&b).expect("the image").to_string()
}

fn ok(r: &mut rdy_engine::Rdy, ops: Value) -> rdy_engine::WriteResult {
    write(r, "work", ops).expect("the fixture write")
}

/// The fixture project with its three removals, closed. Its ids, versions and landings are the
/// same on every run.
pub fn review_project() -> tempfile::TempDir {
    seeded(true)
}

/// [`review_project`] before its removals: the same ids, versions and landings, with no removal
/// request.
pub fn review_project_before_removals() -> tempfile::TempDir {
    seeded(false)
}

fn seeded(removals: bool) -> tempfile::TempDir {
    rdy_model::seed_ids_for_tests(SEED);
    let clock = pin_clock(CLOCK_MS);
    let dir = build(removals);
    drop(clock);
    rdy_model::unseed_ids_for_tests();
    dir
}

fn build(removals: bool) -> tempfile::TempDir {
    let (dir, mut r) = project();
    let (light_a, light_r, light_c, light_c2) = (
        png(&mut r, b"a"),
        png(&mut r, b"r"),
        png(&mut r, b"c"),
        png(&mut r, b"c2"),
    );
    let (light_n, dark_n) = (png(&mut r, b"n"), png(&mut r, b"n dark"));
    let w = ok(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$notes", "fields": {"name": "notes", "title": "Notes", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$dout", "parent": "$notes",
             "fields": {"title": "Outside ruling", "statement": "An outside ruling.", "status": "proposed", "built": "0.1.0"}},
            {"op": "create", "type": "decision", "id": "$droot",
             "fields": {"title": "Root ruling", "statement": "A ruling at the root.", "status": "proposed", "built": "0.1.0"}},
            {"op": "create", "type": "doc", "id": "$intro", "fields": {"name": "intro", "title": "Intro", "doc_type": "page"}},
            {"op": "create", "type": "folder", "id": "$guides", "fields": {"name": "guides", "title": "Guides"}},
            {"op": "create", "type": "doc", "id": "$old", "parent": "$guides", "fields": {"name": "old", "title": "Old", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$old", "body": "Old words."}
        ]),
    );
    let id = |k: &str| w.created[k].clone();
    let (admin, intro, guides, old) = (id("$admin"), id("$intro"), id("$guides"), id("$old"));
    let (notes, dout, droot) = (id("$notes"), id("$dout"), id("$droot"));
    let w = ok(
        &mut r,
        json!([
            {"op": "create", "type": "section", "id": "$start", "parent": intro, "fields": {"title": "Start"}},
            {"op": "create", "type": "prose", "id": "$p1", "parent": "$start", "body": "Run `rdy` first."},
            {"op": "create", "type": "decision", "id": "$d1", "parent": "$start",
             "fields": {"title": "Keep rows", "statement": "Keep `rows` in order.", "status": "proposed", "built": "0.1.0"}},
            {"op": "create", "type": "note", "parent": "$start", "body": "Planned note.", "fields": {"planned": true}},
            {"op": "create", "type": "figure", "id": "$fa", "parent": intro,
             "fields": {"blob": light_a, "media_type": "image/png", "alt": "Figure A"}},
            {"op": "create", "type": "figure", "id": "$fr", "parent": intro,
             "fields": {"blob": light_r, "media_type": "image/png", "alt": "Figure R"}},
            {"op": "create", "type": "figure", "id": "$fc", "parent": intro,
             "fields": {"blob": light_c, "media_type": "image/png", "alt": "Figure C"}},
            {"op": "create", "type": "prose", "parent": intro, "body": format!("See {{{{{notes}}}}}.")},
            {"op": "create", "type": "prose", "parent": intro, "body": format!("Per {{{{{dout}}}}}.")},
            {"op": "create", "type": "claim", "parent": intro, "body": "The outside claim.",
             "links": {"decided_by": [{"target": dout}]}}
        ]),
    );
    let id = |k: &str| w.created[k].clone();
    let (start, p1, d1) = (id("$start"), id("$p1"), id("$d1"));
    let (fr, fc) = (id("$fr"), id("$fc"));
    ok(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "parent": admin,
            "fields": {"key": PROFILE, "title": "Review", "audience": "org", "planned": "hide",
                       "select": [intro, guides]}}]),
    );
    land(&mut r, "work").expect("the first landing");
    r.tag(&approver(), "v1.0.0", None, None).expect("v1.0.0");

    let w = ok(
        &mut r,
        json!([
            {"op": "update", "id": p1, "body": "Run `rdy init` first, then read the guide."},
            {"op": "create", "type": "decision", "parent": start,
             "fields": {"title": "Keep rows by id", "statement": "Keep `rows` in id order.", "status": "proposed", "built": "0.1.0"},
             "links": {"supersedes": [{"target": d1}]}},
            {"op": "create", "type": "decision", "id": "$dn", "parent": start,
             "fields": {"title": "Name pages", "statement": "Name each page.", "status": "proposed", "built": "0.1.0"}},
            {"op": "create", "type": "claim", "id": "$c2", "parent": start, "body": "The root claim.",
             "links": {"decided_by": [{"target": droot}]}},
            {"op": "update", "id": fc, "fields": {"blob": light_c2}},
            {"op": "delete", "id": fr},
            {"op": "update", "id": guides, "fields": {"title": "User guides"}},
            {"op": "move", "id": old},
            {"op": "create", "type": "doc", "id": "$setup", "parent": guides, "fields": {"name": "setup", "title": "Setup", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$install", "parent": "$setup", "fields": {"title": "Install"}},
            {"op": "create", "type": "prose", "parent": "$install", "body": "Install with `cargo`."},
            {"op": "create", "type": "decision", "parent": "$install",
             "fields": {"title": "Pin versions", "statement": "Pin each version.", "status": "proposed", "built": "0.1.0"}},
            {"op": "create", "type": "figure", "parent": "$install",
             "fields": {"blob": light_n, "media_type": "image/png", "dark_blob": dark_n,
                        "dark_media_type": "image/png", "alt": "Figure N"}},
            {"op": "create", "type": "prose", "parent": "$install", "body": format!("Also see {{{{{notes}}}}}.")},
            {"op": "create", "type": "note", "parent": "$install", "body": "Later planned note.", "fields": {"planned": true}}
        ]),
    );
    let (setup, named, claim) = (
        w.created["$setup"].clone(),
        w.created["$dn"].clone(),
        w.created["$c2"].clone(),
    );
    land(&mut r, "work").expect("the second landing");
    r.tag(&approver(), "v2.0.0", None, None).expect("v2.0.0");

    if !removals {
        r.close().expect("close");
        return dir;
    }
    for target in [claim, named, setup] {
        r.redact_in(
            &writer(),
            "redact",
            RedactInput {
                targets: vec![target],
                reason_class: "secret".into(),
                reason: "a leaked value".into(),
                dry_run: false,
            },
        )
        .expect("the removal request");
    }
    land(&mut r, "redact").expect("the removals");
    r.close().expect("close");
    dir
}
