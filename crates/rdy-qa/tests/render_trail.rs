//! The why trail and the agents affordance (why1).
use rdy_engine::Scope;
use rdy_qa::golden::*;

fn trail_of(h: &str, anchor: &str) -> String {
    let i = h.find(&format!("id=\"{anchor}\"")).unwrap();
    let s = &h[i..];
    let j = s.find("<details class=\"why\">").unwrap();
    let k = s[j..].find("</details>").unwrap();
    s[j..j + k].to_string()
}

/// `h` without its "For agents" blocks, which carry the status words as data.
fn without_agents(h: &str) -> String {
    let (mut out, mut rest) = (String::new(), h);
    while let Some(i) = rest.find("<details class=\"agents\"") {
        out.push_str(&rest[..i]);
        let end = rest[i..]
            .find("</details>")
            .map_or(rest.len(), |k| i + k + 10);
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

#[test]
fn why1_the_trail_reads_as_dated_plain_sentences_in_order() {
    let (_d, r, ids) = decided_site();
    let out = tempfile::tempdir().unwrap();
    // D4 is proposed, so it lives in the view `pending`, which canon cannot hold
    rdy_render::render_site(&r.read(), &Scope::View("pending".into()), out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("ledgers.html")).unwrap();
    let t = trail_of(&h, "ledgers-are-atomic-when-settled");
    let items: Vec<&str> = t.split("<li").skip(1).collect();
    assert_eq!(items.len(), 6, "{t}");
    assert!(items[0].contains("<span class=\"trail-when\">23 June 2020</span><span class=\"trail-what\">Ruled: Ledgers are eventually consistent.</span>"), "{t}");
    assert!(items[1].contains("<span class=\"trail-when\">10 January 2021</span><span class=\"trail-what\">Set aside: Ledgers are optional. Options considered: optional and required.</span>"), "{t}");
    assert!(items[2].contains("<span class=\"trail-when\">8 February 2021</span><span class=\"trail-what\">Superseded: Ledgers are eventually consistent gave way to Ledgers are atomic.</span>"), "{t}");
    assert!(items[3].contains("<span class=\"trail-when\">8 February 2021</span><span class=\"trail-what\">Ruled: Ledgers are atomic.</span>"), "{t}");
    assert!(items[4].contains("class=\"open\"") && items[4].contains("<span class=\"trail-when\">Open</span><span class=\"trail-what\">Proposed: Ledgers may be sharded.</span>"), "{t}");
    assert!(items[5].contains("class=\"open\"") && items[5].contains("<span class=\"trail-when\">Open</span><span class=\"trail-what\">Question: Is a ledger a sequence?</span>"), "{t}");
    assert!(t.contains("data-rdy=\"trail\""));
    let _ = ids;
}

#[test]
fn why1_a_revocation_shows_its_reason_without_the_id_and_a_question_is_open() {
    let (_d, r, ids) = decided_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("ledgers.html")).unwrap();
    let t = trail_of(&h, "balance");
    assert!(t.contains("<span class=\"trail-when\">1 March 2021</span><span class=\"trail-what\">Ruled: Balances round down.</span>"));
    assert!(t.contains("<span class=\"trail-when\">2 April 2021</span><span class=\"trail-what\">Withdrawn: Balances round half to even.</span>"), "{t}");
    assert!(
        !t.contains(&ids.d3.to_string()) && !t.contains("Revoke "),
        "{t}"
    );
    assert!(
        !trail_of(&h, "balance").contains("Question:"),
        "the question references R, not T"
    );
}

#[test]
fn why1_blocks_without_events_have_no_pill_and_nothing_shows_a_badge() {
    let (_d, r, _ids) = decided_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("ledgers.html")).unwrap();
    // the lede has no wrapper, the section "Rules" has no decision: up to the next toolbar or
    // block is all either has
    let until_next = |anchor: &str| {
        let rest = &h[h.find(&format!("id=\"{anchor}\"")).unwrap()..];
        let skip = rest.find('>').unwrap_or(0); // past the element's own tag
        let end = ["class=\"tools\"", "data-rdy-block="]
            .iter()
            .filter_map(|m| rest[skip..].find(m))
            .min()
            .map_or(rest.len(), |e| e + skip);
        rest[..end].to_string()
    };
    let plain = until_next("plain-words");
    assert!(plain.contains("Plain words."), "{plain}");
    assert!(
        !until_next("rules").contains("class=\"why\""),
        "{}",
        until_next("rules")
    );
    assert!(!plain.contains("class=\"why\""));
    // the agents' block names a status as data; nothing else on the page does
    let seen = without_agents(&h);
    assert!(
        !seen.contains("class=\"badge")
            && !seen.contains(">live<")
            && !seen.contains(">superseded<")
    );
}

#[test]
fn why1_the_agents_affordance_is_collapsed_and_carries_id_type_lifecycle_and_governing() {
    let (_d, r, ids) = decided_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("ledgers.html")).unwrap();
    let i = h
        .find(&format!("data-rdy-block=\"{}\"", ids.requirement))
        .unwrap();
    let s = &h[i..];
    let a = &s[s
        .find("<details class=\"agents\" data-rdy=\"agents\">")
        .unwrap()..];
    let a = &a[..a.find("</details>").unwrap()];
    assert!(
        a.contains("<summary>For agents</summary>")
            && !a.contains("<details class=\"agents\" data-rdy=\"agents\" open")
    );
    assert!(
        a.contains(&format!(
            "<dt>Id</dt><dd><code>{}</code></dd>",
            ids.requirement
        )) && a.contains("<dt>Type</dt><dd><code>requirement</code></dd>")
    );
    assert!(
        a.contains(&format!("<code>{}</code> <span>live</span>", ids.d1))
            && a.contains(&format!("<code>{}</code> <span>rejected</span>", ids.d2))
    );
    assert!(
        a.contains(&format!(
            "<dt>Permalink</dt><dd><code>go/{}.html</code></dd>",
            ids.requirement
        )) && a.contains("href=\"ledgers.json\"")
    );
    assert!(h.contains("class=\"tool copy-link\" data-href=\"#ledgers-are-atomic-when-settled\" aria-label=\"Copy link to this block\""));
}

/// Review Focus 5.
#[test]
fn why1_hostile_statements_are_escaped_in_the_trail() {
    let (_d, r) = hostile(); // its decision now affects the claim, so the trail renders the hostile statement
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    for p in pages(out.path()) {
        let h = std::fs::read_to_string(&p).unwrap();
        assert!(!h.contains("<script>alert(1)"), "{}", p.display());
    }
    // the claim's page holds its trail, and the trail holds the statement as text
    let h = std::fs::read_to_string(out.path().join("evil/page.html")).unwrap();
    let at = h
        .find("<details class=\"why\">")
        .expect("the governed claim has a Why trail");
    let trail = &h[at..at + h[at..].find("</details>").unwrap()];
    assert!(
        trail.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "{trail}"
    );
}

/// A governed block whose statement ends in punctuation gets no second full stop, and a trail
/// that has events never shows an id (Review Focus 5's neighbour: dates and statements are scanned).
#[test]
fn why1_the_trail_shows_no_id_and_no_second_full_stop() {
    let (_d, r, ids) = decided_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("ledgers.html")).unwrap();
    for anchor in ["ledgers-are-atomic-when-settled", "balance"] {
        let t = trail_of(&h, anchor);
        assert!(!t.contains("..") && !t.contains("?."), "{t}");
        for id in [
            &ids.d0,
            &ids.d1,
            &ids.d2,
            &ids.d3,
            &ids.d3_revoker,
            &ids.q,
            &ids.requirement,
        ] {
            assert!(!t.contains(&id.to_string()), "{id} in {t}");
        }
    }
}
