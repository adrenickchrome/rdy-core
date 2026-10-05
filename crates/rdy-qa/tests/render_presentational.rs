//! A presentational block renders without marker, pill or governing row (pres1).
use rdy_engine::Scope;
use rdy_qa::golden::*;

/// The markup of the block `id`: from its wrapper to the next block's, at most 1,200 bytes, cut
/// at a character boundary.
fn block_slice<'a>(h: &'a str, id: &str) -> &'a str {
    let i = h
        .find(&format!("data-rdy-block=\"{id}\""))
        .unwrap_or_else(|| panic!("no block {id}"));
    let mut end = h.len().min(i + 1200);
    if let Some(next) = h[i + 1..end].find("data-rdy-block=") {
        end = i + 1 + next;
    }
    while !h.is_char_boundary(end) {
        end -= 1;
    }
    &h[i..end]
}

/// A type with a rendering of its own (`note`, a callout) shows no marker when a pack makes it
/// presentational (the Task 14 carry-forward).
#[test]
fn pres1_a_presentational_note_has_no_marker() {
    let (dir, r, ids) = project();
    // the store is locked while a handle is open
    drop(r);
    let pack = PRESENTATIONAL_PACK.replace(
        "  section:",
        "  note: { prefix: n, traits: [presentational], body: required }\n  section:",
    );
    std::fs::write(dir.path().join(".rdy/packs/p.yaml"), pack).unwrap();
    std::fs::write(
        dir.path().join(".rdy/config.yaml"),
        "pack: p\nname: Presentational\n",
    )
    .unwrap();
    let r = rdy_engine::Rdy::open(dir.path()).unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("tutorial/adventure.html")).unwrap();
    let b = block_slice(&h, ids.presentational_note.as_str());
    assert!(
        b.contains("The sample output in this tutorial") && !b.contains("class=\"marker\""),
        "{b}"
    );
    // the types the pack leaves alone keep their markers
    assert!(h.contains("<span class=\"marker\">Example</span>"), "{h}");
}

#[test]
fn pres1_presentational_blocks_render_plainly_and_are_flagged_in_json() {
    let (_d, r, ids) = presentational_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("t.html")).unwrap();
    let b = block_slice(&h, ids.aside.as_str());
    assert!(
        b.contains("<p>Decorative.</p>")
            && !b.contains("class=\"marker\"")
            && !b.contains("<details class=\"why\">")
            && !b.contains("<dt>Governed by</dt>"),
        "{b}"
    );
    let j: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.path().join("t.json")).unwrap()).unwrap();
    let a = j["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == ids.aside.to_string())
        .unwrap();
    assert_eq!(a["presentational"], true);
}

/// Neither a presentational aside nor a presentational note shows a trail, an agents' block or
/// a Copy ref button, though decisions govern both (Task 11's carry-forward).
#[test]
fn pres1_asides_and_notes_get_no_trail_agents_or_copy_ref() {
    let dir = tempfile::tempdir().unwrap();
    drop(rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap());
    let pack = PRESENTATIONAL_PACK.replace(
        "  section:",
        "  note: { prefix: n, traits: [presentational], body: required }\n  section:",
    );
    std::fs::write(dir.path().join(".rdy/packs/p.yaml"), pack).unwrap();
    std::fs::write(
        dir.path().join(".rdy/config.yaml"),
        "pack: p\nname: Presentational\n",
    )
    .unwrap();
    let mut r = rdy_engine::Rdy::open(dir.path()).unwrap();
    let base = write(&mut r, "work", serde_json::json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "t", "title": "T", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "aside", "id": "$a", "parent": "$s", "body": "Decorative."},
        {"op": "create", "type": "note", "id": "$n", "parent": "$s", "body": "Margin."}
    ]))
    .unwrap();
    let (section, aside, note) = (
        base.created["$s"].to_string(),
        base.created["$a"].to_string(),
        base.created["$n"].to_string(),
    );
    let decision = |id: &str, target: &str| {
        serde_json::json!({"op": "create", "type": "decision", "id": id, "parent": section,
            "fields": {"statement": "Stay decorative", "status": "accepted",
                       "decided_at": "2021-02-08T10:00:00Z", "recorded_at": "2021-02-08T10:00:00Z"},
            "links": {"affects": [{"target": target}]}})
    };
    write_as(
        &approver(),
        &mut r,
        "work",
        serde_json::json!([decision("$d1", &aside), decision("$d2", &note)]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("t.html")).unwrap();
    assert!(h.contains("Decorative.") && h.contains("Margin."), "{h}");
    assert!(
        !h.contains("class=\"why\"") && !h.contains("<dt>Governed by</dt>"),
        "{h}"
    );
    for id in [&aside, &note] {
        assert!(
            !h.contains(&format!("<code>{id}</code>")),
            "{id} shows in an agents block"
        );
    }
    // only the section has a reference to copy; the three blocks each have a link
    assert_eq!(h.matches("class=\"tool copy-ref\"").count(), 1, "{h}");
    assert_eq!(h.matches("class=\"tool copy-link\"").count(), 3, "{h}");
}
