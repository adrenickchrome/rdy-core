use crate::IdError;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    pub fn of(bytes: &[u8]) -> Hash {
        Hash(*blake3::hash(bytes).as_bytes())
    }
    /// The first 12 hex digits.
    pub fn short(&self) -> String {
        let mut buf = [0u8; 12];
        hex_into(&self.0[..6], &mut buf);
        ascii(&buf).to_string()
    }
}

/// `bytes` as lowercase hex into `out` (twice as long). perf4: a briefing formats a hash per
/// covered block and per tension, and one `write!` per byte cost about a microsecond a hash.
fn hex_into(bytes: &[u8], out: &mut [u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, b) in bytes.iter().enumerate() {
        out[2 * i] = HEX[usize::from(b >> 4)];
        out[2 * i + 1] = HEX[usize::from(b & 0xf)];
    }
}

/// `buf`, hex digits only, as text.
fn ascii(buf: &[u8]) -> &str {
    std::str::from_utf8(buf).expect("hex digits are ASCII")
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buf = [0u8; 64];
        hex_into(&self.0, &mut buf);
        f.write_str(ascii(&buf))
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", self.short())
    }
}

impl FromStr for Hash {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 64 {
            return Err(IdError::BadHash(s.to_string()));
        }
        // Validate that all characters are lowercase hex digits [0-9a-f]
        for c in s.chars() {
            if !matches!(c, '0'..='9' | 'a'..='f') {
                return Err(IdError::BadHash(s.to_string()));
            }
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let pair = std::str::from_utf8(chunk).map_err(|_| IdError::BadHash(s.to_string()))?;
            out[i] = u8::from_str_radix(pair, 16).map_err(|_| IdError::BadHash(s.to_string()))?;
        }
        Ok(Hash(out))
    }
}

#[cfg(test)]
mod perf4_tests {
    use super::Hash;

    #[test]
    fn perf4_a_hash_prints_as_lowercase_hex_and_short_is_its_first_12_digits() {
        let h = Hash::of(b"perf4");
        let slow: String = h.0.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(h.to_string(), slow);
        assert_eq!(h.short(), slow[..12]);
        assert_eq!(slow.parse::<Hash>().unwrap(), h);
        let edges = Hash([
            0x00, 0x0f, 0xf0, 0xff, 0x9a, 0xa9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0x01,
        ]);
        assert_eq!(edges.short(), "000ff0ff9aa9");
        assert!(edges.to_string().ends_with("0001"));
    }
}
