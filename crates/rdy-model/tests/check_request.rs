//! The stored check request: wire order, mode ordinals, option combinations, bounds and unknown
//! keys. No product writer exists yet.

use rdy_model::check_request::*;
use rdy_model::{ActorRun, BlockId, CROCKFORD, Hash, Provenance, RelayEvidence, Role};
use serde_yaml_ng::Value;

fn id(n: u32) -> BlockId {
    let mut body = [b'0'; 5];
    let mut rest = n;
    for slot in body.iter_mut().rev() {
        *slot = CROCKFORD[(rest % 32) as usize];
        rest /= 32;
    }
    BlockId::parse(&format!("b-{}", std::str::from_utf8(&body).unwrap())).unwrap()
}

fn h(n: u8) -> Hash {
    Hash([n; 32])
}

fn varint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fn s(text: &str) -> Vec<u8> {
    let mut out = varint(text.len() as u64);
    out.extend_from_slice(text.as_bytes());
    out
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

fn some(bytes: Vec<u8>) -> Vec<u8> {
    cat(&[vec![1], bytes])
}

fn roundtrip<T>(value: &T, bytes: &[u8])
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(
        postcard::to_allocvec(value).unwrap(),
        bytes,
        "encoded bytes"
    );
    assert_eq!(
        &postcard::from_bytes::<T>(bytes).unwrap(),
        value,
        "decoded value"
    );
    let yaml = serde_yaml_ng::to_string(value).unwrap();
    assert_eq!(
        &serde_yaml_ng::from_str::<T>(&yaml).unwrap(),
        value,
        "yaml form"
    );
}

fn bare() -> CheckRequest {
    CheckRequest {
        check: "review".into(),
        subject: CheckSubject {
            blocks: vec![],
            change: None,
            version: None,
            code: None,
        },
        scope: "all".into(),
        case_hash: h(9),
        mode: CheckMode::Author,
        reason: None,
        requested_by: Provenance {
            actor: "pat".into(),
            run: None,
            role: Role::Writer,
            at_ms: 1,
        },
        relayed_by: None,
        relay: None,
    }
}

fn full() -> CheckRequest {
    CheckRequest {
        check: "independent-review".into(),
        subject: CheckSubject {
            blocks: vec![id(1), id(2)],
            change: Some(h(1)),
            version: Some(h(2)),
            code: Some(CheckCode {
                repo: "repo".into(),
                from: "main".into(),
                to: "topic".into(),
            }),
        },
        scope: "design".into(),
        case_hash: h(3),
        mode: CheckMode::Independent,
        reason: Some("a second look".into()),
        requested_by: Provenance {
            actor: "pat".into(),
            run: Some("r1".into()),
            role: Role::Approver,
            at_ms: 1,
        },
        relayed_by: Some(ActorRun {
            actor: "agent".into(),
            run: None,
        }),
        relay: Some(RelayEvidence {
            quoted: "yes".into(),
            session_ref: "s-1".into(),
            question: Some("go?".into()),
        }),
    }
}

#[test]
fn ok3_check_request_roundtrips_every_field() {
    // every option present, mode Independent (ordinal 1)
    let r = full();
    r.validate().unwrap();
    roundtrip(
        &r,
        &cat(&[
            s("independent-review"),
            // subject: blocks, change, version, code
            varint(2),
            s(id(1).as_str()),
            s(id(2).as_str()),
            some(h(1).0.to_vec()),
            some(h(2).0.to_vec()),
            some(cat(&[s("repo"), s("main"), s("topic")])),
            s("design"),
            h(3).0.to_vec(),
            vec![1],
            some(s("a second look")),
            // requested_by: actor, run, role (Approver = 3), at_ms (zigzag 1 = 2)
            cat(&[s("pat"), some(s("r1")), vec![3], vec![2]]),
            // relayed_by, relay
            some(cat(&[s("agent"), vec![0]])),
            some(cat(&[s("yes"), s("s-1"), some(s("go?"))])),
        ]),
    );

    // every option absent, mode Author (ordinal 0)
    let b = bare();
    b.validate().unwrap();
    roundtrip(
        &b,
        &cat(&[
            s("review"),
            vec![0, 0, 0, 0],
            s("all"),
            h(9).0.to_vec(),
            vec![0],
            vec![0],
            cat(&[s("pat"), vec![0], vec![1], vec![2]]),
            vec![0],
            vec![0],
        ]),
    );
    roundtrip(&CheckMode::Author, &[0]);
    roundtrip(&CheckMode::Independent, &[1]);
    assert!(CheckMode::Author < CheckMode::Independent);
    assert_eq!(
        serde_yaml_ng::to_string(&CheckMode::Independent)
            .unwrap()
            .trim(),
        "independent"
    );

    // the options coexist in any combination, or stay absent
    for mask in 0..16u8 {
        let mut r = bare();
        if mask & 1 != 0 {
            r.subject.blocks = vec![id(1)];
        }
        if mask & 2 != 0 {
            r.subject.change = Some(h(1));
        }
        if mask & 4 != 0 {
            r.subject.version = Some(h(2));
        }
        if mask & 8 != 0 {
            r.subject.code = Some(CheckCode {
                repo: "r".into(),
                from: "a".into(),
                to: "b".into(),
            });
        }
        r.validate()
            .unwrap_or_else(|e| panic!("subject mask {mask}: {e}"));
    }
}

#[test]
fn ok3_check_request_bounds() {
    let edit = |f: &dyn Fn(&mut CheckRequest)| {
        let mut r = full();
        f(&mut r);
        r.validate()
    };
    // check, scope and repo: 1-256 bytes
    for (n, ok) in [(0, false), (1, true), (256, true), (257, false)] {
        let t = "n".repeat(n);
        assert_eq!(edit(&|r| r.check = t.clone()).is_ok(), ok, "check {n}");
        assert_eq!(edit(&|r| r.scope = t.clone()).is_ok(), ok, "scope {n}");
        let repo = |r: &mut CheckRequest| r.subject.code.as_mut().unwrap().repo = t.clone();
        assert_eq!(edit(&repo).is_ok(), ok, "repo {n}");
    }
    assert_eq!(edit(&|r| r.check.clear()), Err("invalid check name"));
    assert_eq!(edit(&|r| r.scope.clear()), Err("invalid check scope"));
    // from and to: 1-64 bytes; this carrier parses no revision
    for (n, ok) in [(0, false), (1, true), (64, true), (65, false)] {
        let t = "v".repeat(n);
        assert_eq!(
            edit(&|r| r.subject.code.as_mut().unwrap().from = t.clone()).is_ok(),
            ok,
            "from {n}"
        );
        assert_eq!(
            edit(&|r| r.subject.code.as_mut().unwrap().to = t.clone()).is_ok(),
            ok,
            "to {n}"
        );
    }
    assert_eq!(
        edit(&|r| r.subject.code.as_mut().unwrap().to.clear()),
        Err("invalid check code")
    );
    assert_eq!(
        edit(&|r| r.subject.code.as_mut().unwrap().from = "not a git revision ~^:".into()),
        Ok(())
    );
    // a present reason: 1-2,000 scalar values after trim, 8,000 bytes
    let bad = Err("invalid check reason");
    assert_eq!(edit(&|r| r.reason = Some(String::new())), bad);
    assert_eq!(edit(&|r| r.reason = Some("  ".into())), bad);
    assert_eq!(edit(&|r| r.reason = Some("x".repeat(2000))), Ok(()));
    assert_eq!(edit(&|r| r.reason = Some("x".repeat(2001))), bad);
    assert_eq!(edit(&|r| r.reason = Some("\u{1F600}".repeat(2000))), Ok(()));
    assert_eq!(
        edit(&|r| r.reason = Some(format!("{} ", "\u{1F600}".repeat(2000)))),
        bad
    );
    assert_eq!(edit(&|r| r.reason = None), Ok(()));

    // subject blocks: sorted, distinct, at most 100,000; zero is allowed
    let blocks = Err("check blocks are not sorted and distinct");
    assert_eq!(edit(&|r| r.subject.blocks = vec![]), Ok(()));
    assert_eq!(edit(&|r| r.subject.blocks = vec![id(2), id(1)]), blocks);
    assert_eq!(edit(&|r| r.subject.blocks = vec![id(1), id(1)]), blocks);
    assert_eq!(
        edit(&|r| r.subject.blocks = (0..100_000).map(id).collect()),
        Ok(())
    );
    assert_eq!(
        edit(&|r| r.subject.blocks = (0..100_001).map(id).collect()),
        blocks
    );

    // the relay fields are both present or both absent
    let differ = Err("check relay fields differ");
    assert_eq!(edit(&|r| r.relay = None), differ);
    assert_eq!(edit(&|r| r.relayed_by = None), differ);
    assert_eq!(edit(&|r| (r.relayed_by, r.relay) = (None, None)), Ok(()));
    // no actor grammar applies to the relay or to the requester
    assert_eq!(
        edit(&|r| {
            r.relayed_by = Some(ActorRun {
                actor: "Any Text!".into(),
                run: Some(String::new()),
            });
            r.requested_by.actor = String::new();
        }),
        Ok(())
    );

    // the first failed check wins, in wire order
    let mut r = full();
    r.check.clear();
    r.subject.blocks = vec![id(2), id(1)];
    r.scope.clear();
    r.reason = Some(String::new());
    r.relay = None;
    assert_eq!(r.validate(), Err("invalid check name"));
    r.check = "c".into();
    assert_eq!(
        r.validate(),
        Err("check blocks are not sorted and distinct")
    );
    r.subject.blocks.clear();
    assert_eq!(r.validate(), Err("invalid check scope"));
    r.scope = "s".into();
    assert_eq!(r.validate(), Err("invalid check reason"));
    r.reason = None;
    assert_eq!(r.validate(), Err("check relay fields differ"));
}

fn mappings(v: &Value) -> usize {
    match v {
        Value::Mapping(m) => 1 + m.values().map(mappings).sum::<usize>(),
        Value::Sequence(items) => items.iter().map(mappings).sum(),
        Value::Tagged(t) => mappings(&t.value),
        _ => 0,
    }
}

fn add_unknown_key(v: &mut Value, n: &mut usize) -> bool {
    match v {
        Value::Mapping(m) => {
            if *n == 0 {
                m.insert(Value::String("zzz_unknown".into()), Value::Bool(true));
                return true;
            }
            *n -= 1;
            m.iter_mut().any(|(_, child)| add_unknown_key(child, n))
        }
        Value::Sequence(items) => items.iter_mut().any(|child| add_unknown_key(child, n)),
        Value::Tagged(t) => add_unknown_key(&mut t.value, n),
        _ => false,
    }
}

#[test]
fn ok3_check_request_inputs_reject_unknown_keys() {
    let value = serde_yaml_ng::to_value(full()).unwrap();
    let total = mappings(&value);
    // the request, its subject, the code and the existing actor and relay records
    assert_eq!(total, 6);
    serde_yaml_ng::from_value::<CheckRequest>(value.clone()).unwrap();
    // every mapping refuses an unknown key: the request, subject, code, requested_by,
    // relayed_by and relay (mapping order is the field order)
    for n in 0..total {
        let mut edited = value.clone();
        let mut at = n;
        assert!(add_unknown_key(&mut edited, &mut at));
        assert!(
            serde_yaml_ng::from_value::<CheckRequest>(edited).is_err(),
            "mapping {n}"
        );
    }
    // the existing records keep their historical, permissive behaviour
    let mut p = serde_yaml_ng::to_value(full().requested_by).unwrap();
    add_unknown_key(&mut p, &mut 0);
    serde_yaml_ng::from_value::<Provenance>(p).unwrap();
    let mut a = serde_yaml_ng::to_value(full().relayed_by.unwrap()).unwrap();
    add_unknown_key(&mut a, &mut 0);
    serde_yaml_ng::from_value::<ActorRun>(a).unwrap();
    let mut r = serde_yaml_ng::to_value(full().relay.unwrap()).unwrap();
    add_unknown_key(&mut r, &mut 0);
    serde_yaml_ng::from_value::<RelayEvidence>(r).unwrap();
    // absent options still read as None
    let mut bare = serde_yaml_ng::to_value(full()).unwrap();
    let m = bare.as_mapping_mut().unwrap();
    m.remove("relayed_by");
    m.remove("relay");
    m.remove("reason");
    let bare: CheckRequest = serde_yaml_ng::from_value(bare).unwrap();
    assert!(bare.relayed_by.is_none() && bare.relay.is_none());
}
