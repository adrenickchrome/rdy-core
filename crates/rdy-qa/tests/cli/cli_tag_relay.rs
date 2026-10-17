//! di9 (D-KQ4CA, D-DGCH7, D-HA7YZ, O19, O32): the CLI's parity for the relayed tag and the
//! relayed proposal of the person's view. An agent session relays with `--relayed-by`,
//! `--evidence`, `--question` and `--session`; a human session cannot relay.
use rdy_engine::Rdy;
use rdy_model::AnswerAct;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_store::kinds;
use serde_json::json;

/// Runs `f` on the project with the daemon stopped (it holds the store while it runs).
fn with_rdy<T>(s: &Served, f: impl FnOnce(&mut Rdy) -> T) -> T {
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let out = f(&mut r);
    r.close().unwrap();
    out
}

/// A served project with one landing.
fn landed() -> Served {
    let s = Served::new();
    with_rdy(&s, |r| {
        seed_doc(r, "seed");
        land(r, "seed").unwrap();
    });
    s
}

/// The kinds of the frames of the log's last batch.
fn last_batch_kinds(r: &Rdy) -> Vec<u8> {
    let st = r.read();
    let mut last = vec![];
    st.reader()
        .for_each_batch(st.seq(), |b| {
            last = b.frames.iter().map(|f| f.kind()).collect();
            Ok(())
        })
        .unwrap();
    last
}

fn text(o: &std::process::Output) -> (bool, String) {
    (
        o.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

#[test]
fn di9_rdy_tag_relays_from_an_agent_session() {
    let s = landed();
    // with no relay flags an agent session still cannot tag (O19)
    let (ok, v) = s.json_as("agent:relay", Some("r1"), &["--json", "tag", "v0.1.0"]);
    assert!(!ok);
    assert_eq!(
        (v["code"].as_str(), v["message"].as_str()),
        (
            Some("E_PERMISSION"),
            Some("agent sessions cannot tag or import")
        ),
        "{v}"
    );
    // half a relay never reaches rdy: clap pairs the flags
    let (ok, out) = text(&s.rdy_as(
        "agent:relay",
        Some("r1"),
        &["tag", "v0.1.0", "--relayed-by", "agent:relay/r1"],
    ));
    assert!(!ok && out.contains("--evidence"), "{out}");
    let (ok, out) = text(&s.rdy_as(
        "agent:relay",
        Some("r1"),
        &["tag", "v0.1.0", "--evidence", "yes"],
    ));
    assert!(!ok && out.contains("--relayed-by"), "{out}");
    // the relay names the agent as itself
    let (ok, v) = s.json_as(
        "agent:relay",
        Some("r1"),
        &[
            "--json",
            "tag",
            "v0.1.0",
            "--relayed-by",
            "agent:other",
            "--evidence",
            "yes",
        ],
    );
    assert!(!ok);
    assert_eq!(v["code"], "E_PERMISSION", "{v}");
    // the relayed tag: the text names the relay
    let (ok, out) = text(&s.rdy_as(
        "agent:relay",
        Some("r1"),
        &[
            "tag",
            "v0.1.0",
            "-m",
            "first",
            "--relayed-by",
            "agent:relay/r1",
            "--evidence",
            "yes, tag it",
            "--question",
            "Tag v0.1.0?",
            "--session",
            "chat-3",
        ],
    ));
    assert!(ok, "{out}");
    assert!(
        out.starts_with("version v0.1.0 → landing ")
            && out.trim_end().ends_with(", relayed by agent:relay/r1"),
        "{out}"
    );
    let (ok, v) = s.json(&["--json", "fsck"]);
    assert!(ok, "{v}");
    with_rdy(&s, |r| {
        let k = last_batch_kinds(r);
        assert!(
            k.contains(&kinds::TAG) && k.contains(&kinds::ANSWER),
            "one batch: {k:?}"
        );
        let (_, a) = r
            .read()
            .reader()
            .answer_of("tag:v0.1.0")
            .unwrap()
            .expect("the tag's answer");
        assert_eq!(a.act, AnswerAct::Tag);
        assert_eq!(a.person.actor, "human:qa");
        let by = a.relayed_by.unwrap();
        assert_eq!(
            (by.actor.as_str(), by.run.as_deref()),
            ("agent:relay", Some("r1"))
        );
        let ev = a.relay_evidence.unwrap();
        assert_eq!(
            (
                ev.quoted.as_str(),
                ev.question.as_deref(),
                ev.session_ref.as_str()
            ),
            ("yes, tag it", Some("Tag v0.1.0?"), "chat-3")
        );
        let tags = r.read().versions().unwrap();
        assert_eq!(tags[0].tagged_by, "human:qa", "the person tags");
    });
    // the JSON reply names the person and the relay
    let (ok, v) = s.json_as(
        "agent:relay",
        Some("r1"),
        &[
            "--json",
            "tag",
            "v0.2.0",
            "--relayed-by",
            "agent:relay",
            "--evidence",
            "and v0.2.0",
        ],
    );
    assert!(ok, "{v}");
    assert_eq!(v["tagged_by"], "human:qa", "{v}");
    assert_eq!(
        v["relayed_by"],
        json!({"actor": "agent:relay", "run": "r1"})
    );
    assert!(v["answer"].is_string(), "{v}");
}

#[test]
fn di9_a_human_session_cannot_relay_a_tag() {
    let s = landed();
    let (ok, v) = s.json(&[
        "--json",
        "tag",
        "v0.1.0",
        "--relayed-by",
        "human:qa",
        "--evidence",
        "yes",
    ]);
    assert!(!ok);
    assert_eq!(v["code"], "E_PERMISSION", "{v}");
    // the person tags plainly: no relay, no answer
    let (ok, v) = s.json(&["--json", "tag", "v0.1.0"]);
    assert!(ok, "{v}");
    assert_eq!(v["tagged_by"], "human:qa", "{v}");
    assert!(v["relayed_by"].is_null() && v["answer"].is_null(), "{v}");
    with_rdy(&s, |r| {
        assert!(r.read().reader().answer_of("tag:v0.1.0").unwrap().is_none());
        assert!(!last_batch_kinds(r).contains(&kinds::ANSWER));
    });
}

#[test]
fn di9_rdy_propose_relays_the_persons_view() {
    let s = landed();
    with_rdy(&s, |r| {
        write(
            r,
            "work",
            json!([{"op": "create", "type": "doc", "fields": {"name": "plans", "title": "Plans", "doc_type": "page"}}]),
        )
        .unwrap();
    });
    // without a relay the person's view stays theirs
    let (ok, v) = s.json_as(
        "agent:relay",
        None,
        &["--json", "propose", "--title", "Plans", "--view", "work"],
    );
    assert!(!ok);
    assert_eq!(v["code"], "E_VIEW_OWNER", "{v}");
    // a human session cannot relay a proposal
    let (ok, v) = s.json(&[
        "--json",
        "propose",
        "--title",
        "Plans",
        "--relayed-by",
        "human:qa",
        "--evidence",
        "yes",
    ]);
    assert!(!ok);
    assert_eq!(v["code"], "E_PERMISSION", "{v}");
    // half a relay never reaches rdy
    let (ok, out) = text(&s.rdy_as(
        "agent:relay",
        None,
        &[
            "propose",
            "--title",
            "Plans",
            "--view",
            "work",
            "--evidence",
            "yes",
        ],
    ));
    assert!(!ok && out.contains("--relayed-by"), "{out}");
    let (ok, out) = text(&s.rdy_as(
        "agent:relay",
        None,
        &[
            "propose",
            "--title",
            "Plans",
            "--view",
            "work",
            "--summary",
            "The plans doc.",
            "--relayed-by",
            "agent:relay",
            "--evidence",
            "yes, propose my work",
            "--question",
            "Propose work?",
        ],
    ));
    assert!(ok, "{out}");
    let first = out.lines().next().unwrap();
    assert!(
        first.starts_with("proposal P-") && first.ends_with("(1 changes), relayed by agent:relay"),
        "{out}"
    );
    let (ok, v) = s.json(&["--json", "fsck"]);
    assert!(ok, "{v}");
    with_rdy(&s, |r| {
        let k = last_batch_kinds(r);
        assert!(
            k.contains(&kinds::PROPOSAL) && k.contains(&kinds::ANSWER),
            "one batch: {k:?}"
        );
        let (_, a) = r
            .read()
            .reader()
            .answer_of("propose:work")
            .unwrap()
            .expect("the proposal's answer");
        assert_eq!(a.act, AnswerAct::Propose);
        assert_eq!(a.person.actor, "human:qa");
        assert_eq!(a.relay_evidence.unwrap().session_ref, "cli");
        let p = r.read().reader().proposal(&a.targets[0]).unwrap();
        assert_eq!(
            (p.view.as_str(), p.provenance.actor.as_str()),
            ("work", "human:qa")
        );
    });
}
