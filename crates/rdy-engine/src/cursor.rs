//! The page cursor of a paged read (O11): AE1's grammar, `<64 hex>:<offset>`. `rdy_search` keeps
//! its own copy with its own texts.
use rdy_model::Hash;

use crate::error::*;

/// "<64 hex>:<offset>".
#[allow(dead_code)] // the diff and query pages call it
pub(crate) fn make(token: &Hash, offset: usize) -> String {
    format!("{token}:{offset}")
}

/// AE1's grammar: 64 lowercase hex characters, `:`, 1 to 9 ASCII digits. Else `E_BAD_INPUT`.
#[allow(dead_code)] // the diff and query pages call it
pub(crate) fn parse(text: &str, tool: &str) -> Result<(Hash, usize), RdyError> {
    let bad = || {
        RdyError::new(
            E_BAD_INPUT,
            format!("page is not a cursor from {tool}'s next"),
            "pass next from the previous page, unchanged",
        )
    };
    let (token, offset) = text.split_once(':').ok_or_else(bad)?;
    let token = token.parse::<Hash>().map_err(|_| bad())?;
    if !(1..=9).contains(&offset.len()) || !offset.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let offset = offset.parse().map_err(|_| bad())?;
    Ok((token, offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> Hash {
        Hash::of(b"dq1")
    }

    #[test]
    fn dq1_a_cursor_round_trips() {
        let t = token();
        assert_eq!(parse(&make(&t, 7), "rdy_diff").unwrap(), (t, 7));
        assert_eq!(make(&t, 7), format!("{t}:7"));
    }

    #[test]
    fn dq1_a_cursor_keeps_ae1s_grammar() {
        let hex = token().to_string();
        let bad = [
            "word".to_string(),
            format!("{}:1", &hex[..63]),
            format!("{}:1", hex.to_uppercase()),
            format!("{hex}:"),
            format!("{hex}:1234567890"),
            format!("{hex}:-1"),
            format!("{hex}:+1"),
            hex.clone(),
            String::new(),
        ];
        for text in &bad {
            let e = parse(text, "rdy_diff").unwrap_err();
            assert_eq!(e.code, E_BAD_INPUT, "{text}");
            assert_eq!(
                e.message, "page is not a cursor from rdy_diff's next",
                "{text}"
            );
            assert_eq!(
                e.fix, "pass next from the previous page, unchanged",
                "{text}"
            );
        }
        // nine digits are the limit, and the tool name is the one passed
        assert_eq!(
            parse(&format!("{hex}:999999999"), "rdy_diff").unwrap().1,
            999_999_999
        );
        let e = parse("word", "rdy_query").unwrap_err();
        assert_eq!(e.message, "page is not a cursor from rdy_query's next");
    }
}
