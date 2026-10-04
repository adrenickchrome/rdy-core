use rdy_model::{BlockId, Hash, IdError};

#[test]
fn hash_is_blake3_and_round_trips_through_hex() {
    let h = Hash::of(b"rdy");
    assert_eq!(h.0, *blake3::hash(b"rdy").as_bytes());
    let s = h.to_string();
    assert_eq!(s.len(), 64);
    assert_eq!(s.parse::<Hash>().unwrap(), h);
    assert_eq!(h.short(), s[..12]);
}

#[test]
fn bad_hash_hex_is_rejected() {
    assert!(matches!("zz".parse::<Hash>(), Err(IdError::BadHash(_))));
}

#[test]
fn new_ids_have_prefix_and_five_crockford_chars() {
    let id = BlockId::new("D");
    let s = id.as_str();
    assert!(s.starts_with("D-"));
    assert_eq!(s.len(), 7);
    assert!(s[2..].chars().all(|c| "0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(c)));
    assert_eq!(id.prefix(), "D");
}

#[test]
fn parse_accepts_valid_and_rejects_invalid() {
    assert_eq!(BlockId::parse("b-7KQ2M").unwrap().as_str(), "b-7KQ2M");
    assert!(matches!(BlockId::parse("7KQ2M"), Err(IdError::BadPrefix(_))));
    assert!(matches!(BlockId::parse("toolong-7KQ2M"), Err(IdError::BadPrefix(_))));
    assert!(matches!(BlockId::parse("b-7KQ2"), Err(IdError::BadBody(_))));
    assert!(matches!(BlockId::parse("b-7KQ2I"), Err(IdError::BadBody(_)))); // I is not Crockford
}

#[test]
fn ids_serialize_as_strings_and_validate_on_deserialize() {
    let id = BlockId::parse("t-MEDRG").unwrap();
    let yaml = serde_yaml_ng::to_string(&id).unwrap();
    assert_eq!(yaml.trim(), "t-MEDRG");
    assert!(serde_yaml_ng::from_str::<BlockId>("nope").is_err());
}

#[test]
fn hash_postcard_round_trip() {
    let h = Hash::of(b"x");
    let bytes = postcard::to_allocvec(&h).unwrap();
    assert_eq!(postcard::from_bytes::<Hash>(&bytes).unwrap(), h);
}
