//! se8: `rdy resolve --accept` and what `rdy show` and `rdy tensions` show of a reviewer's advice,
//! through the daemon.
use rdy_engine::tensions::*;
use rdy_engine::{Rdy, Scope};
use rdy_model::*;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::writer;
use rdy_qa::tensions::*;

fn content(r: &Rdy, id: &BlockId) -> Hash {
    let m = r.read().materialized(&Scope::Inflight).unwrap();
    m.get(id).unwrap().content_hash_with(r.store().tracking())
}

#[test]
fn se8_rdy_resolve_accept_and_the_listings_show_the_advice() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let sp = spec_doc(&mut r);
    put(&mut r, "a", upd(&sp.c, "C contradicts B."));
    let mut items = vec![
        PairItem {
            block: sp.b.clone(),
            content: content(&r, &sp.b),
        },
        PairItem {
            block: sp.c.clone(),
            content: content(&r, &sp.c),
        },
    ];
    items.sort();
    let key = pair_key(&items);
    let review = Review {
        subject: ReviewSubject::Pair {
            key,
            items: items.clone(),
        },
        case_hash: Some(Hash([9; 32])),
        verdict: Verdict::Tension,
        reason: String::new(),
        findings: vec![Finding {
            block: sp.b.clone(),
            note: "B and C cannot both hold".into(),
        }],
        provenance: Provenance {
            actor: "agent:r".into(),
            run: Some("r".into()),
            role: Role::Reviewer,
            at_ms: 1,
        },
    };
    let first = SemanticTension {
        key,
        items,
        review: Hash::of(&postcard::to_allocvec(&review).unwrap()),
        advice: Some("it contradicts the gate rule".into()),
        blocking: false,
        relay: None,
        relayed_by: None,
        prev: None,
        provenance: review.provenance.clone(),
    };
    r.record_verdict(&review, Some(&first)).unwrap();
    let tn = tension_label(&one(&r, TensionKind::Semantic).id);
    let p = r.propose(&writer(), "a", "a", None).unwrap();
    let (pref, digest) = (p.r#ref, p.advised_digest.unwrap());
    r.close().unwrap();

    let out = s.rdy(&["show", &pref]);
    let shown = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        shown.contains(&format!(
            "advised: {tn} by agent:r: it contradicts the gate rule"
        )),
        "{shown}"
    );
    // FR1 (ruling 1): show prints the command with the digest of the advised tensions it lists
    assert!(
        shown.contains(&format!(
            "accept the advice with: rdy resolve --accept {pref} --digest {digest}"
        )),
        "{shown}"
    );
    // --accept needs --digest; --evidence needs --relayed-by; --accept goes alone
    let out = s.rdy(&["resolve", "--accept", &pref]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = s.rdy(&[
        "resolve",
        "--accept",
        &pref,
        "--digest",
        &digest,
        "--evidence",
        "yes",
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = s.rdy(&["resolve", &tn, "--accept", &pref, "--digest", &digest]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // di8 (C5, D-67ZN7): a person accepts plainly and never names a relayer
    let long = format!("agent:{}", "w".repeat(251));
    let out = s.rdy(&[
        "resolve",
        "--accept",
        &pref,
        "--digest",
        &digest,
        "--evidence",
        "yes",
        "--relayed-by",
        &long,
    ]);
    assert!(
        !out.status.success()
            && String::from_utf8_lossy(&out.stderr).contains(
                "a person answers as themselves; only the relaying agent records a relay"
            ),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = s.rdy(&["resolve", "--accept", &pref, "--digest", &digest]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (ok, v) = s.json(&["tensions", "--kind", "semantic", "--json"]);
    assert!(ok, "{v}");
    let new = v["tensions"][0]["id"].as_str().unwrap().to_string();
    assert_ne!(new, tn);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&format!(
            "accepted {new} (was {tn}); it blocks until a side's content changes"
        )),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains(&format!("now blocking in view a ({pref}): {new}")),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let out = s.rdy(&["tensions"]);
    let listed = String::from_utf8_lossy(&out.stdout).to_string();
    for want in [
        "semantic",
        "blocking",
        "judged by agent:r: B and C cannot both hold",
        "advises blocking: it contradicts the gate rule",
        "accepted by human:qa",
    ] {
        assert!(listed.contains(want), "{want}: {listed}");
    }
}
