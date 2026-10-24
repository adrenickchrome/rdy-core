//! The stored dispute and its resolution: wire order, outcome ordinals, reason bounds and
//! unknown keys.

use rdy_model::dispute::*;
use rdy_model::{Hash, Provenance, Role};
use serde_yaml_ng::Value;

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

fn zigzag(n: i64) -> Vec<u8> {
    varint(((n << 1) ^ (n >> 63)) as u64)
}

fn s(text: &str) -> Vec<u8> {
    let mut out = varint(text.len() as u64);
    out.extend_from_slice(text.as_bytes());
    out
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
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

fn who(actor: &str, run: Option<&str>, at_ms: i64) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: run.map(str::to_string),
        role: Role::Reviewer,
        at_ms,
    }
}

fn who_bytes(actor: &str, run: Option<&str>, at_ms: i64) -> Vec<u8> {
    cat(&[
        s(actor),
        match run {
            Some(r) => cat(&[vec![1], s(r)]),
            None => vec![0],
        },
        vec![2],
        zigzag(at_ms),
    ])
}

fn dispute(reason: &str) -> Dispute {
    Dispute {
        landing: h(1),
        opened_by: who("pat", None, 5),
        reason: reason.into(),
        at_ms: 9_000,
    }
}

#[test]
fn ok3_dispute_roundtrips_all_outcomes() {
    // wire order: landing, opened_by, reason, at_ms; at_ms need not equal opened_by.at_ms
    let d = Dispute {
        landing: h(1),
        opened_by: who("pat", Some("run-1"), 5),
        reason: "the approval named the wrong version".into(),
        at_ms: -3,
    };
    d.validate().unwrap();
    roundtrip(
        &d,
        &cat(&[
            h(1).0.to_vec(),
            who_bytes("pat", Some("run-1"), 5),
            s("the approval named the wrong version"),
            zigzag(-3),
        ]),
    );

    // outcome ordinals, names and order
    let outcomes = [
        (DisputeOutcome::Upheld, 0u8, "upheld"),
        (DisputeOutcome::Dismissed, 1, "dismissed"),
        (DisputeOutcome::Withdrawn, 2, "withdrawn"),
    ];
    for (outcome, ordinal, name) in outcomes {
        roundtrip(&outcome, &[ordinal]);
        assert_eq!(serde_yaml_ng::to_string(&outcome).unwrap().trim(), name);
        for correction in [None, Some(h(4))] {
            let r = DisputeResolution {
                dispute: h(3),
                outcome,
                by: who("lee", None, 11),
                correction,
            };
            r.validate().unwrap();
            roundtrip(
                &r,
                &cat(&[
                    h(3).0.to_vec(),
                    vec![ordinal],
                    who_bytes("lee", None, 11),
                    match correction {
                        Some(c) => cat(&[vec![1], c.0.to_vec()]),
                        None => vec![0],
                    },
                ]),
            );
        }
    }
    assert!(DisputeOutcome::Upheld < DisputeOutcome::Dismissed);
    assert!(DisputeOutcome::Dismissed < DisputeOutcome::Withdrawn);

    // actor text keeps its historical form: no input grammar applies
    let odd = Dispute {
        opened_by: who("Not A Valid Actor!", Some(""), 0),
        ..dispute("r")
    };
    odd.validate().unwrap();
    roundtrip(
        &odd,
        &cat(&[
            h(1).0.to_vec(),
            who_bytes("Not A Valid Actor!", Some(""), 0),
            s("r"),
            zigzag(9_000),
        ]),
    );
}

#[test]
fn ok3_dispute_reason_bounds() {
    let bad = Err("invalid dispute reason");
    assert_eq!(dispute("").validate(), bad);
    assert_eq!(dispute(" \t\n ").validate(), bad);
    assert_eq!(dispute("x").validate(), Ok(()));
    assert_eq!(dispute("  x  ").validate(), Ok(()));
    // 2,000 scalar values pass; 2,001 fail; padding does not count toward the 2,000
    assert_eq!(dispute(&"x".repeat(2000)).validate(), Ok(()));
    assert_eq!(dispute(&"x".repeat(2001)).validate(), bad);
    assert_eq!(
        dispute(&format!("  {}  ", "x".repeat(2000))).validate(),
        Ok(())
    );
    // scalar values, not bytes: 2,000 four-byte scalars are 8,000 bytes and pass
    assert_eq!(dispute(&"\u{1F600}".repeat(2000)).validate(), Ok(()));
    assert_eq!(dispute(&"\u{1F600}".repeat(2001)).validate(), bad);
    // 8,001 bytes fail even when the trimmed text has 2,000 scalar values
    assert_eq!(
        dispute(&format!("{} ", "\u{1F600}".repeat(2000))).validate(),
        bad
    );
    // the record keeps the reason as written
    let d = dispute("  padded  ");
    d.validate().unwrap();
    assert_eq!(d.reason, "  padded  ");
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
fn ok3_dispute_inputs_reject_unknown_keys() {
    // only the dispute's own keys are refused: Provenance is an existing record type
    let value = serde_yaml_ng::to_value(dispute("r")).unwrap();
    let mut edited = value.clone();
    assert!(add_unknown_key(&mut edited, &mut 0));
    assert!(serde_yaml_ng::from_value::<Dispute>(edited).is_err());
    serde_yaml_ng::from_value::<Dispute>(value).unwrap();

    let resolution = DisputeResolution {
        dispute: h(3),
        outcome: DisputeOutcome::Upheld,
        by: who("lee", None, 1),
        correction: Some(h(4)),
    };
    let value = serde_yaml_ng::to_value(&resolution).unwrap();
    assert!(mappings(&value) >= 2);
    let mut edited = value.clone();
    assert!(add_unknown_key(&mut edited, &mut 0));
    assert!(serde_yaml_ng::from_value::<DisputeResolution>(edited).is_err());
    serde_yaml_ng::from_value::<DisputeResolution>(value).unwrap();
}
