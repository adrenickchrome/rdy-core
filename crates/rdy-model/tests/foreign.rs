//! Task 1 of the repo-links plan: the shared foreign values.
use rdy_model::foreign::*;
use rdy_model::{BlockId, Hash, RepoId};

fn repo(n: u8) -> RepoId {
    RepoId([n; 16])
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn fid(n: u8, s: &str) -> ForeignId {
    ForeignId::new(repo(n), bid(s))
}

fn version(s: &str) -> ForeignVersion {
    ForeignVersion {
        id: bid(s),
        version: Hash::of(b"v"),
        content: Hash::of(b"c"),
        encoding: 2,
        derived: ForeignDerived {
            status: Some("accepted".into()),
            lifecycle: None,
            superseded_by: vec![bid("D-00002")],
        },
    }
}

#[test]
fn rpl1_foreign_id_has_repo_and_block() {
    let a = fid(1, "D-00001");
    let b = fid(2, "D-00001");
    assert_ne!(a, b, "the repo is part of the identity");
    assert!(a < b, "ordered by repo first");
    // postcard: the 16 repo bytes, then the block id
    let bytes = postcard::to_allocvec(&a).unwrap();
    assert_eq!(&bytes[..16], &[1u8; 16]);
    assert_eq!(postcard::from_bytes::<ForeignId>(&bytes).unwrap(), a);
    // json: one text, and it parses back
    let text = serde_json::to_string(&a).unwrap();
    assert_eq!(text, format!("\"{}/D-00001\"", repo(1)));
    assert_eq!(serde_json::from_str::<ForeignId>(&text).unwrap(), a);
    assert!(serde_json::from_str::<ForeignId>("\"nope\"").is_err());

    let mut parts = ForeignParts::default();
    parts.links.insert(
        "cites".into(),
        vec![ForeignLink {
            target: a.clone(),
            part: Some("body".into()),
        }],
    );
    parts.pins.insert(
        a.clone(),
        ForeignPin {
            content: Hash::of(b"x"),
            encoding: 2,
        },
    );
    let json = serde_json::to_string(&parts).unwrap();
    assert_eq!(serde_json::from_str::<ForeignParts>(&json).unwrap(), parts);
    let pc = postcard::to_allocvec(&parts).unwrap();
    assert_eq!(postcard::from_bytes::<ForeignParts>(&pc).unwrap(), parts);
}

#[test]
fn rpl1_sources_and_events_have_closed_shapes() {
    // frozen variant order: the first postcard byte of an enum is its index
    let link = ForeignSource::Link {
        link: "up".into(),
        site: "s".into(),
        at_ms: 5,
    };
    let adr = ForeignSource::Adr {
        source: "adrs".into(),
        revision: "r1".into(),
    };
    assert_eq!(postcard::to_allocvec(&link).unwrap()[0], 0);
    assert_eq!(postcard::to_allocvec(&adr).unwrap()[0], 1);
    let delivery = ForeignEvent::Delivery {
        prev: None,
        publication: ForeignPublication {
            tag: "t".into(),
            profile: "p".into(),
            profile_version: Some(Hash::of(b"profile")),
        },
        set: Some(ForeignSet::Team("core".into())),
        added: vec![version("D-00001")],
        changed: vec![],
        removed: vec![bid("D-00003")],
        shells: vec![],
        selection: Hash::of(b"s"),
    };
    let revoked = ForeignEvent::Revoked {
        prev: Hash::of(b"p"),
    };
    let removal = ForeignEvent::Removal {
        proof: Hash::of(b"q"),
        versions: vec![(bid("D-00001"), Hash::of(b"v"))],
    };
    for (i, e) in [&delivery, &revoked, &removal].into_iter().enumerate() {
        assert_eq!(postcard::to_allocvec(e).unwrap()[0] as usize, i);
    }
    assert_eq!(
        postcard::to_allocvec(&ForeignSet::Organization).unwrap()[0],
        0
    );
    assert_eq!(
        postcard::to_allocvec(&ForeignSet::Team("t".into())).unwrap()[0],
        1
    );
    for event in [delivery, revoked, removal] {
        for source in [link.clone(), adr.clone()] {
            let entry = ForeignEntry {
                repo: repo(7),
                source,
                event: event.clone(),
            };
            let json = serde_json::to_string(&entry).unwrap();
            assert_eq!(from_json::<ForeignEntry>(&json).unwrap(), entry);
            let pc = postcard::to_allocvec(&entry).unwrap();
            assert_eq!(postcard::from_bytes::<ForeignEntry>(&pc).unwrap(), entry);
        }
    }
    // closed: an unknown key is refused, with the nearest known key
    let pin = format!("{{\"content\":\"{}\",\"encodng\":1}}", Hash::of(b"x"));
    assert_eq!(
        from_json::<ForeignPin>(&pin).unwrap_err(),
        ForeignInputError::UnknownKeyDidYouMean {
            key: "encodng".into(),
            suggestion: "encoding".into()
        }
    );
    let far = r#"{"links":{},"pins":{},"zzzzzz":1}"#;
    assert!(matches!(
        from_json::<ForeignParts>(far).unwrap_err(),
        ForeignInputError::UnknownKey { .. }
    ));
    let entry =
        r#"{"repo":"x","source":{"link":{"link":"a","site":"b","at_ms":1,"extra":1}},"event":{}}"#;
    assert!(from_json::<ForeignEntry>(entry).is_err());
    // nested: every other value is valid, so only the unknown key fails
    let nested = format!(
        r#"{{"repo":"{}","source":{{"link":{{"link":"a","site":"b","at_ms":1,"extra":1}}}},"event":{{"revoked":{{"prev":"{}"}}}}}}"#,
        repo(7),
        Hash::of(b"p")
    );
    assert!(matches!(
        from_json::<ForeignEntry>(&nested).unwrap_err(),
        ForeignInputError::UnknownKey { ref key, .. } | ForeignInputError::UnknownKeyDidYouMean { ref key, .. } if key == "extra"
    ));
    let fixed = nested.replace(r#","extra":1"#, "");
    assert!(
        from_json::<ForeignEntry>(&fixed).is_ok(),
        "the base input is valid"
    );
}

#[test]
fn rpl1_adr_source_id_is_domain_separated() {
    let a = adr_source_id("docs");
    assert_eq!(a, adr_source_id("docs"), "stable");
    assert_ne!(a, adr_source_id("docs2"), "a rename is a new source");
    let mut h = blake3::Hasher::new();
    h.update(b"rdy-adr-source-1\0docs");
    assert_eq!(a.0[..], h.finalize().as_bytes()[..16]);
    let plain = blake3::hash(b"docs");
    assert_ne!(a.0[..], plain.as_bytes()[..16], "the domain changes the id");
    // the selection identity is domain-separated and takes the pairs in the order given (id order)
    let (x, xv) = (bid("D-00001"), Hash::of(b"1"));
    let (y, yv) = (bid("D-00002"), Hash::of(b"2"));
    let xy = selection_identity([(&x, &xv), (&y, &yv)]).unwrap();
    assert_eq!(xy, selection_identity([(&x, &xv), (&y, &yv)]).unwrap());
    assert_ne!(xy, selection_identity([(&y, &yv), (&x, &xv)]).unwrap());
    let mut h = blake3::Hasher::new();
    h.update(SELECTION_IDENTITY_DOMAIN);
    h.update(&1u32.to_le_bytes());
    h.update(&0u32.to_le_bytes());
    assert_eq!(
        selection_identity(std::iter::empty()).unwrap().0,
        *h.finalize().as_bytes()
    );
}

#[test]
fn rpl1_publication_profile_version_is_a_hash_or_absent() {
    for profile_version in [Some(Hash::of(b"a profile")), None] {
        let p = ForeignPublication {
            tag: "t".into(),
            profile: "p".into(),
            profile_version,
        };
        let json = serde_json::to_string(&p).unwrap();
        match profile_version {
            Some(h) => assert!(json.contains(&format!("\"profile_version\":\"{h}\""))),
            None => assert!(json.contains("\"profile_version\":null")),
        }
        assert_eq!(from_json::<ForeignPublication>(&json).unwrap(), p);
        let pc = postcard::to_allocvec(&p).unwrap();
        assert_eq!(postcard::from_bytes::<ForeignPublication>(&pc).unwrap(), p);
    }
}

#[test]
fn rpl1_removal_versions_are_hex_in_json() {
    let h = Hash::of(b"v");
    let removal = ForeignEvent::Removal {
        proof: Hash::of(b"q"),
        versions: vec![(bid("D-00001"), h)],
    };
    let json = serde_json::to_string(&removal).unwrap();
    assert!(json.contains(&format!("[\"D-00001\",\"{h}\"]")), "{json}");
    assert_eq!(from_json::<ForeignEvent>(&json).unwrap(), removal);
    let pc = postcard::to_allocvec(&removal).unwrap();
    assert_eq!(postcard::from_bytes::<ForeignEvent>(&pc).unwrap(), removal);
}

#[test]
fn rpl1_delivery_prev_may_be_omitted() {
    let json = format!(
        r#"{{"delivery":{{"publication":{{"tag":"t","profile":"p","profile_version":null}},"set":null,"added":[],"changed":[],"removed":[],"selection":"{}"}}}}"#,
        Hash::of(b"s")
    );
    match from_json::<ForeignEvent>(&json).unwrap() {
        ForeignEvent::Delivery { prev, .. } => assert_eq!(prev, None),
        other => panic!("{other:?}"),
    }
}

#[test]
fn rpl1_entry_repo_is_hex_in_json() {
    let entry = ForeignEntry {
        repo: repo(7),
        source: ForeignSource::Adr {
            source: "adrs".into(),
            revision: "r".into(),
        },
        event: ForeignEvent::Revoked {
            prev: Hash::of(b"p"),
        },
    };
    let json = serde_json::to_string(&entry).unwrap();
    assert!(
        json.contains(&format!("\"repo\":\"{}\"", repo(7))),
        "{json}"
    );
    assert_eq!(from_json::<ForeignEntry>(&json).unwrap(), entry);
}
