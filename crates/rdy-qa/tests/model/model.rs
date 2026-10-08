use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};
use rdy_model::*;
use rdy_qa::fixtures::cases;

fn cfg(n: u32) -> Config {
    Config {
        cases: cases(n),
        rng_seed: RngSeed::Fixed(0x5eed_0001),
        failure_persistence: None,
        ..Config::default()
    }
}

#[test]
fn ord2_ten_thousand_appends_stay_short() {
    let mut last: Option<String> = None;
    for i in 0..10_000 {
        let k = order_between(last.as_deref(), None).unwrap();
        assert!(k.len() <= 12, "append {i}: {k}");
        if let Some(l) = &last {
            assert!(l < &k, "{l} < {k}")
        }
        assert!(valid_order_key(&k));
        last = Some(k);
    }
    assert_eq!(order_between(None, None).unwrap(), "a1");
}

#[test]
fn ord1_equal_bounds_are_an_error_not_a_panic() {
    assert_eq!(
        order_between(Some("l"), Some("l")),
        Err(OrderError::NoRoom("l".into(), "l".into()))
    );
    assert_eq!(
        order_between(Some("m"), Some("l")),
        Err(OrderError::NoRoom("m".into(), "l".into()))
    );
}

#[test]
fn ord3_malformed_keys_and_the_cap() {
    for bad in ["", "a0", "a-b", "é", &"z".repeat(25)] {
        assert!(!valid_order_key(bad), "{bad:?}");
        assert!(
            matches!(
                order_between(Some(bad), None),
                Err(OrderError::Malformed(_))
            ),
            "{bad:?}"
        );
    }
    // repeated insert-after one anchor hits the cap and is refused, never over-long
    let (lo, mut hi) = ("a1".to_string(), "a2".to_string());
    let mut refused = false;
    for _ in 0..400 {
        match order_between(Some(&lo), Some(&hi)) {
            Ok(k) => {
                assert!(k.len() <= MAX_ORDER_KEY_LEN);
                hi = k;
            }
            Err(OrderError::TooDeep) => {
                refused = true;
                break;
            }
            Err(e) => panic!("{e:?}"),
        }
    }
    assert!(refused, "the cap is reached within 400 same-spot inserts");
}

#[test]
fn ord2_appends_after_m1_keys_stay_greater() {
    for old in ["V", "k", "s", "w", "y", "z", "zV", "zzzk", "a1V", "b0z"] {
        let k = order_between(Some(old), None).unwrap();
        assert!(old < k.as_str(), "{old} < {k}");
    }
}

#[test]
fn ord3_rebalance_keys_respects_anchors() {
    let items = vec![
        None,
        Some("k".to_string()),
        None,
        None,
        Some("k1".to_string()),
        None,
    ];
    let keys = rebalance_keys(&items).unwrap();
    assert_eq!(keys[1], "k");
    assert_eq!(keys[4], "k1");
    assert!(keys.windows(2).all(|w| w[0] < w[1]), "{keys:?}");
    assert!(keys.iter().all(|k| valid_order_key(k)));
    // 1,000 movables after nothing: short keys
    let keys = rebalance_keys(&vec![None; 1000]).unwrap();
    assert!(keys.iter().all(|k| k.len() <= 4));
    // movables between tied anchors have no room
    assert!(rebalance_keys(&[Some("k".into()), None, Some("k".into())]).is_err());
}

proptest! {
    #![proptest_config(cfg(256))]
    #[test]
    fn model_between_is_valid_and_strictly_between(steps in proptest::collection::vec((0u8..3, any::<u16>()), 1..120)) {
        let mut keys: Vec<String> = vec![order_between(None, None).unwrap()];
        for (kind, pick) in steps {
            let i = pick as usize % keys.len();
            let (lo, hi) = match kind {
                0 => (keys.last().cloned(), None),
                1 => (None, Some(keys[0].clone())),
                _ => (Some(keys[i].clone()), keys.get(i + 1).cloned()),
            };
            match order_between(lo.as_deref(), hi.as_deref()) {
                Ok(k) => {
                    prop_assert!(valid_order_key(&k));
                    if let Some(l) = &lo { prop_assert!(l < &k); }
                    if let Some(h) = &hi { prop_assert!(&k < h); }
                    let pos = keys.partition_point(|x| x < &k);
                    keys.insert(pos, k);
                }
                Err(OrderError::TooDeep) => {}
                Err(e) => prop_assert!(false, "unexpected {e:?}"),
            }
        }
    }

    #[test]
    fn model_no_input_panics(a in "\\PC{0,30}", b in "\\PC{0,30}") {
        let _ = order_between(Some(&a), Some(&b));
        let _ = order_between(Some(&a), None);
        let _ = order_between(None, Some(&b));
        let _ = valid_order_key(&a);
    }
}
