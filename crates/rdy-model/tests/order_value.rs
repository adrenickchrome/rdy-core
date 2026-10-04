use proptest::prelude::*;
use rdy_model::{FieldValue, MAX_ORDER_KEY_LEN, OrderError, order_between};

#[test]
fn first_key_is_the_first_counter_key() {
    assert_eq!(order_between(None, None).unwrap(), "a1");
}

#[test]
fn repeated_inserts_stay_ordered() {
    let mut keys = vec![order_between(None, None).unwrap()];
    for _ in 0..200 {
        let k = order_between(Some(keys.last().unwrap()), None).unwrap();
        assert!(k > *keys.last().unwrap());
        keys.push(k);
    }
    // Same-spot prepends and inserts grow the key until the 24-character cap refuses them.
    let mut first = keys[0].clone();
    let mut prepends = 0;
    loop {
        match order_between(None, Some(&first)) {
            Ok(k) => {
                assert!(k < first && k.len() <= MAX_ORDER_KEY_LEN);
                first = k;
                prepends += 1;
            }
            Err(OrderError::TooDeep) => break,
            Err(e) => panic!("{e:?}"),
        }
    }
    assert!(prepends >= 100, "only {prepends} prepends before the cap");
    let (mut lo, hi) = (keys[0].clone(), keys[1].clone());
    let mut inserts = 0;
    loop {
        match order_between(Some(&lo), Some(&hi)) {
            Ok(k) => {
                assert!(lo < k && k < hi, "{lo} < {k} < {hi}");
                assert!(!k.ends_with('0') && k.len() <= MAX_ORDER_KEY_LEN);
                lo = k;
                inserts += 1;
            }
            Err(OrderError::TooDeep) => break,
            Err(e) => panic!("{e:?}"),
        }
    }
    assert!(inserts >= 100, "only {inserts} inserts before the cap");
}

proptest! {
    #[test]
    fn between_is_strictly_between(steps in proptest::collection::vec(0u8..3, 1..60)) {
        let mut keys: Vec<String> = vec![order_between(None, None).unwrap()];
        for s in steps {
            let i = (s as usize) % keys.len();
            let lo = if s == 0 { None } else { Some(keys[i].clone()) };
            let hi = if s == 0 { keys.first().cloned() } else { keys.get(i + 1).cloned() };
            let k = order_between(lo.as_deref(), hi.as_deref()).unwrap();
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
