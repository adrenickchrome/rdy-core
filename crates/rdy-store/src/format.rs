//! The store's format record: the highest log format it was written with and the oldest reader
//! that can open it. The log is the truth (refs `meta/log_format` and `meta/min_reader`); the
//! `FORMAT` file mirrors them so a newer store is refused before the index is opened.
use crate::error::StoreError;
use crate::fsutil;
use std::path::Path;

/// The log format this binary writes.
pub const LOG_FORMAT: u32 = 2;
/// The highest `min_reader` this binary opens.
pub const READER_FORMAT: u32 = 2;
/// The mirror file in the store directory.
pub const FORMAT_FILE: &str = "FORMAT";
/// The log ref holding `log_format`, in ASCII decimal.
pub const REF_LOG_FORMAT: &str = "meta/log_format";
/// The log ref holding `min_reader`, in ASCII decimal.
pub const REF_MIN_READER: &str = "meta/min_reader";

const FIRST_LINE: &str = "rdy store format";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub log_format: u32,
    pub min_reader: u32,
}

impl Format {
    pub const CURRENT: Format = Format {
        log_format: LOG_FORMAT,
        min_reader: READER_FORMAT,
    };

    pub fn to_text(&self) -> String {
        format!(
            "{FIRST_LINE}\nlog_format {}\nmin_reader {}\n",
            self.log_format, self.min_reader
        )
    }

    /// Parses `FORMAT` text. Unknown `<key> <number>` lines are ignored (a newer rdy may add
    /// keys); both known keys are required.
    pub fn parse(text: &str) -> Result<Format, String> {
        let mut lines = text.lines();
        if lines.next() != Some(FIRST_LINE) {
            return Err(format!("the first line is not {FIRST_LINE:?}"));
        }
        let (mut log_format, mut min_reader) = (None, None);
        for line in lines {
            let number = line
                .split_once(' ')
                .and_then(|(key, n)| decode_u32(n.as_bytes()).map(|n| (key, n)));
            match number {
                Some(("log_format", n)) => log_format = Some(n),
                Some(("min_reader", n)) => min_reader = Some(n),
                Some(_) => {}
                None => return Err(format!("bad line {line:?}: expected `<key> <number>`")),
            }
        }
        Ok(Format {
            log_format: log_format.ok_or("the key log_format is missing")?,
            min_reader: min_reader.ok_or("the key min_reader is missing")?,
        })
    }
}

/// Reads `<store_dir>/FORMAT`: None if the file does not exist.
pub fn read_format_file(store_dir: &Path) -> Result<Option<Format>, StoreError> {
    let path = store_dir.join(FORMAT_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let unreadable = |reason: String| StoreError::Format {
        message: format!(
            "the store's FORMAT file at {} is unreadable: {reason}",
            path.display()
        ),
        fix: "restore FORMAT from backup, or delete it and reopen: rdy rewrites it from the log"
            .into(),
    };
    let text = String::from_utf8(bytes).map_err(|_| unreadable("it is not UTF-8 text".into()))?;
    Format::parse(&text).map(Some).map_err(unreadable)
}

/// Writes `<store_dir>/FORMAT` durably.
pub fn write_format_file(store_dir: &Path, f: Format) -> Result<(), StoreError> {
    fsutil::write_durable(&store_dir.join(FORMAT_FILE), f.to_text().as_bytes())?;
    Ok(())
}

/// Refuses a store that needs a newer reader than this binary.
pub fn check_reader(f: Format, store_dir: &Path) -> Result<(), StoreError> {
    if f.min_reader > READER_FORMAT {
        return Err(StoreError::Format {
            message: format!(
                "store written by a newer rdy: {} needs reader format {}, and this rdy reads up to format {READER_FORMAT}",
                store_dir.display(),
                f.min_reader
            ),
            fix: "upgrade rdy to the version that wrote this store".into(),
        });
    }
    Ok(())
}

pub fn encode_u32(n: u32) -> Vec<u8> {
    n.to_string().into_bytes()
}

/// Strict ASCII decimal: digits only, no sign, no whitespace.
pub fn decode_u32(b: &[u8]) -> Option<u32> {
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(b).ok()?.parse().ok()
}
