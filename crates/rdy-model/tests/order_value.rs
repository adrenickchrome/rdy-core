use proptest::prelude::*;
use rdy_model::{order_between, FieldValue};

#[test]
fn first_key_is_middle_digit() {
    assert_eq!(order_between(None, None), "V");
}

#[test]
fn repeated_inserts_stay_ordered() {
    let mut keys = vec![order_between(None, None)];
    for _ in 0..200 {
        let k = order_between(Some(keys.last().unwrap()), None);
        assert!(k > *keys.last().unwrap());
        keys.push(k);
    }
    let mut first = keys[0].clone();
    for _ in 0..200 {
        let k = order_between(None, Some(&first));
        assert!(k < first);
        first = k;
    }
    let (mut lo, hi) = (keys[0].clone(), keys[1].clone());
    for _ in 0..200 {
        let k = order_between(Some(&lo), Some(&hi));
        assert!(lo < k && k < hi, "{lo} < {k} < {hi}");
        assert!(!k.ends_with('0'));
        lo = k;
    }
}

proptest! {
    #[test]
    fn between_is_strictly_between(steps in proptest::collection::vec(0u8..3, 1..60)) {
        let mut keys: Vec<String> = vec![order_between(None, None)];
        for s in steps {
            let i = (s as usize) % keys.len();
            let lo = if s == 0 { None } else { Some(keys[i].clone()) };
            let hi = if s == 0 { keys.first().cloned() } else { keys.get(i + 1).cloned() };
            let k = order_between(lo.as_deref(), hi.as_deref());
            if let Some(l) = &lo { prop_assert!(k > *l); }
            if let Some(h) = &hi { prop_assert!(k < *h); }
            keys.push(k);
            keys.sort();
        }
    }
}

#[test]
fn field_values_round_trip_yaml_and_postcard() {
    let yaml = "a: 1\nb: [true, \"x\", null]\nc: { d: \"ünïcødé\" }\n";
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).unwrap();
    let fv = FieldValue::from_yaml(&v).unwrap();
    assert_eq!(FieldValue::from_yaml(&fv.to_yaml()).unwrap(), fv);
    let bytes = postcard::to_allocvec(&fv).unwrap();
    assert_eq!(postcard::from_bytes::<FieldValue>(&bytes).unwrap(), fv);
}

#[test]
fn floats_are_rejected_with_a_hint() {
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str("1.5").unwrap();
    let err = FieldValue::from_yaml(&v).unwrap_err();
    assert!(err.contains("string"), "{err}");
}
