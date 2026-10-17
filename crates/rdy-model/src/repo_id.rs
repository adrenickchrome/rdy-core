//! A project's repo id (FW-A2.repo-id D-DAGNB): 16 random bytes, written once when the store is
//! made. A clone keeps it; a fork gets a new one.
use crate::id::{IdError, random_bytes};
use serde::{Deserialize, Serialize};
use std::fmt;

/// A project's repo id: 16 bytes. Postcard writes the 16 bytes; the text is 32 lowercase hex
/// digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RepoId(pub [u8; 16]);

impl RepoId {
    /// A new id: random, or on a seeded thread (`seed_ids_for_tests`) from the thread's own
    /// repo-id stream, which never moves the `BlockId` stream (O19).
    // a default repo id would be a random one, which `Default` should not hide
    #[allow(clippy::new_without_default)]
    pub fn new() -> RepoId {
        let mut b = [0u8; 16];
        random_bytes(&mut b);
        RepoId(b)
    }
}

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for RepoId {
    type Err = IdError;

    /// Exactly 32 hex digits, in either case.
    fn from_str(s: &str) -> Result<RepoId, IdError> {
        let bad = || IdError::BadRepoId(s.to_string());
        if s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad());
        }
        let mut out = [0u8; 16];
        for (i, o) in out.iter_mut().enumerate() {
            *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| bad())?;
        }
        Ok(RepoId(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockId, IdError, seed_ids_for_tests, unseed_ids_for_tests};

    #[test]
    fn ec1_a_repo_id_is_32_lowercase_hex_digits_and_parses_back() {
        let id = RepoId::new();
        let text = id.to_string();
        assert_eq!(text.len(), 32, "{text}");
        assert!(
            text.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{text}"
        );
        assert_eq!(text.parse::<RepoId>(), Ok(id));
        assert_eq!(text.to_uppercase().parse::<RepoId>(), Ok(id), "either case");
        let fixed = RepoId([
            0x00, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xff, 0x10, 0x20, 0x30, 0x40,
            0x50, 0x60,
        ]);
        assert_eq!(fixed.to_string(), "000123456789abcdefff102030405060");
        assert_eq!(
            postcard::to_allocvec(&fixed).unwrap(),
            fixed.0.to_vec(),
            "postcard: the 16 bytes"
        );
    }

    #[test]
    fn ec1_a_repo_id_text_of_another_shape_is_refused() {
        let cases = [
            String::new(),
            "a".repeat(31),
            "a".repeat(33),
            "g".repeat(32),
        ];
        for s in cases {
            assert_eq!(
                s.parse::<RepoId>(),
                Err(IdError::BadRepoId(s.clone())),
                "{s:?}"
            );
        }
        assert_eq!(
            IdError::BadRepoId("x".into()).to_string(),
            "bad repo id \"x\": expected 32 hex digits"
        );
    }

    #[test]
    fn ec1_seeded_threads_draw_the_same_repo_ids() {
        let draw = || {
            std::thread::spawn(|| {
                seed_ids_for_tests(7);
                let ids = (RepoId::new(), RepoId::new());
                unseed_ids_for_tests();
                ids
            })
            .join()
            .unwrap()
        };
        let (a, b) = (draw(), draw());
        assert_eq!(a, b, "two threads seeded alike");
        assert_ne!(a.0, a.1, "a seeded thread's draws differ");
        let (x, y) = std::thread::spawn(|| (RepoId::new(), RepoId::new()))
            .join()
            .unwrap();
        assert_ne!(x, y, "an unseeded thread draws two different ids");
    }

    #[test]
    fn ec1_a_repo_id_draw_leaves_the_block_id_stream_alone() {
        let with = std::thread::spawn(|| {
            seed_ids_for_tests(11);
            let a = BlockId::new("p");
            let _ = RepoId::new();
            let b = BlockId::new("p");
            unseed_ids_for_tests();
            (a, b)
        })
        .join()
        .unwrap();
        let without = std::thread::spawn(|| {
            seed_ids_for_tests(11);
            let a = BlockId::new("p");
            let b = BlockId::new("p");
            unseed_ids_for_tests();
            (a, b)
        })
        .join()
        .unwrap();
        assert_eq!(
            with, without,
            "O19: a repo id never moves the BlockId stream"
        );
    }
}
