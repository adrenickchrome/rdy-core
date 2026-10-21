//! The spelling of a load stop: a landing, a tag or a batch end in a dump chain.
//!
//! The parser owns spelling only. It never looks at a store: a stop is resolved against the
//! physical batches of the dump chain, so a landing that a holder's history dropped from canon
//! still selects its batch.
use crate::error::RdyError;
use crate::version::landing_spelling;

/// The most bytes a stop selector may have, prefix included.
pub const MAX_STOP_BYTES: usize = 1_048_580;
/// The most bytes a tag name may have in a selector.
pub const MAX_TAG_BYTES: usize = 1_048_576;
/// The most files a dump chain may have after its full dump (increments).
pub const MAX_CHAIN_FILES: usize = 4096;

/// The fewest and most hex digits of a landing selector.
const LANDING_MIN: usize = 6;
const LANDING_MAX: usize = 64;

/// Where a load stops: at a landing, at a tag, or at the end of the batch at a log position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DumpStop {
    /// A landing hash or a hex prefix of it, lowercase.
    Landing(String),
    /// A tag name, exactly as written.
    Tag(String),
    /// The end of a batch: its log segment and byte offset.
    Position { segment: u32, offset: u64 },
}

impl DumpStop {
    /// Parses `text`: `landing:<hex>`, `tag:<name>` or `pos:<segment>.<offset>`. The prefixes
    /// are lowercase and exact. The size bound is checked before anything else.
    pub fn parse(text: &str) -> Result<DumpStop, RdyError> {
        if text.len() > MAX_STOP_BYTES {
            return Err(crate::dump_error::stop_limit());
        }
        let bad = || crate::dump_error::stop_syntax(text);
        if let Some(rest) = text.strip_prefix("landing:") {
            if rest.len() < LANDING_MIN || rest.len() > LANDING_MAX {
                return Err(bad());
            }
            return landing_spelling(rest)
                .map(DumpStop::Landing)
                .map_err(|_| bad());
        }
        if let Some(rest) = text.strip_prefix("tag:") {
            if rest.is_empty() || rest.len() > MAX_TAG_BYTES {
                return Err(bad());
            }
            return Ok(DumpStop::Tag(rest.to_string()));
        }
        if let Some(rest) = text.strip_prefix("pos:") {
            let (segment, offset) = rest.split_once('.').ok_or_else(bad)?;
            let segment = segment.parse::<u32>().ok().filter(|_| digits(segment));
            let offset = offset.parse::<u64>().ok().filter(|_| digits(offset));
            return match (segment, offset) {
                (Some(segment), Some(offset)) => Ok(DumpStop::Position { segment, offset }),
                _ => Err(bad()),
            };
        }
        Err(bad())
    }
}

/// True for a nonempty run of ASCII decimal digits (`str::parse` also takes a sign).
fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}
