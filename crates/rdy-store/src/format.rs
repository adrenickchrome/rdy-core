//! The store's format (plan M2a1, fmt2): one number, in the log (ref `meta/store_format`) and in
//! the `FORMAT` file. A store opens only if its `FORMAT` says this binary's number (fmt3): a
//! missing `FORMAT` (an M1 store, or any directory rdy did not create) is refused before the
//! index or the log is opened, and so is a log that disagrees. There is no compatibility between
//! formats, and opening never writes a `FORMAT`: only `Store::create` (rdy init) and
//! `Store::create_holder` do.
use crate::error::StoreError;
use crate::fsutil;
use std::path::Path;

/// The store format this binary reads and writes. 4 (plan M2c): `ViewOpKind::Rebase.conflicts`,
/// `ViewOpKind::Fork.owner`, `RelayEvidence.question`, `LandingRecord.evidence` and
/// `Proposal.canon_at`. 5 (plan M2c3): pins hold content hashes (FW-LP1); Justification::Implements holds a list; Proposal.summary (FW-G7c).
/// 6 (plan M2d1): kinds INTENT and ALT_SET, refs intent/ and alt/.
/// 7 (plan M2d2): kind REVIEW and refs dismiss/.
/// 8 (plan Coverage and confidence): REVIEW subject Mention, verdict Restore, refs mention/; ChangeLinks.imported_by.
/// 9 (plan M2e1): REVIEW subject Pair, verdicts Tension and NoTension, kind TENSION (with advice and a relayed acceptance), refs pair/ and semantic/.
/// 10 (plan M2e2): kind TRANSITION (a batch's tension and suspect moves, founder decision C, D-Z0EMP), and the index's open-tension and open-suspect tables.
/// 11 = ANSWER, answer/, pending/ (Decision intake).
/// 12 = meta/repo_id at the first batch, meta/origin (Export and clone).
/// 13 = kinds REDACTION, REDACTION_RESIDUE and REDACTION_APPLY, and the redaction index
/// (Redaction); it moves in dump format 2, which carries removed payloads as dropped frames.
/// 14 = kind SIGNATURE and the signature index (signing); it moves in dump format 2 as the pair
/// (2, 14).
/// 15 = ref `meta/store_mode` (a holder, D-BPYN8) and a holder's `answer/tag-object:<hash>`
/// refs, one per relayed TAG object (O11 as amended by ruling task-13); no kind and no record
/// encoding changes. It moves in dump format 2 as the pair (2, 15).
pub const STORE_FORMAT: u32 = 15;
/// The first store format that a dump carries (dl2, D-69AY3): dump format 1 carries 10 to 12,
/// dump format 2 carries 13 to 15. A store of this format or a later one can move to a newer rdy
/// with dump and load; an earlier one cannot.
pub const FIRST_DUMP_STORE_FORMAT: u32 = 10;
/// The mirror file in the store directory.
pub const FORMAT_FILE: &str = "FORMAT";
/// The log ref holding `store_format`, in ASCII decimal.
pub const REF_STORE_FORMAT: &str = "meta/store_format";

const FIRST_LINE: &str = "rdy store format";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub store_format: u32,
}

impl Format {
    pub const CURRENT: Format = Format {
        store_format: STORE_FORMAT,
    };

    pub fn to_text(&self) -> String {
        format!("{FIRST_LINE}\nstore_format {}\n", self.store_format)
    }

    /// Parses `FORMAT` text. Unknown `<key> <number>` lines are ignored (a later rdy may add
    /// keys); without a `store_format` line the result is `None`.
    pub fn parse(text: &str) -> Result<Option<Format>, String> {
        let mut lines = text.lines();
        if lines.next() != Some(FIRST_LINE) {
            return Err(format!("the first line is not {FIRST_LINE:?}"));
        }
        let mut store_format = None;
        for line in lines {
            let number = line
                .split_once(' ')
                .and_then(|(key, n)| decode_u32(n.as_bytes()).map(|n| (key, n)));
            match number {
                Some(("store_format", n)) => store_format = Some(n),
                Some(_) => {}
                None => return Err(format!("bad line {line:?}: expected `<key> <number>`")),
            }
        }
        Ok(store_format.map(|store_format| Format { store_format }))
    }
}

/// The fix for a refusal with no store format number to go by.
const RE_CREATE: &str = "re-create it with this rdy";

/// The in-place move of a project across a store format change (final ruling 1, worded by
/// final-round1 ruling 2), in order. `found` is the store format the project has now: the rdy
/// that wrote the store may have no `rdy dump` (0.2.0 and earlier), so the fix names a release
/// that has one and reads that format. The CLI help and the `rdy_load` text give it with `<n>`.
pub fn move_with_dump(found: &str) -> String {
    format!(
        "close every session on this project; with rdy 0.3.0 or a later rdy that reads store \
         format {found}, run rdy dump --out <file>, then rdy serve --stop; move .rdy/store aside; \
         then run rdy load <file> . with this rdy"
    )
}

/// The fix text for a store that names format `found` when this binary reads `current` (dl2, a
/// closed rule): a newer format needs a newer rdy; a format from `FIRST_DUMP_STORE_FORMAT` up to
/// the current one moves with dump and load; an earlier format is re-created.
pub fn fix_for(found: u32, current: u32) -> String {
    if found > current {
        "a newer rdy wrote this store: open it with that rdy or a later one".into()
    } else if found >= FIRST_DUMP_STORE_FORMAT && found < current {
        move_with_dump(&found.to_string())
    } else {
        RE_CREATE.into()
    }
}

/// The one refusal for a store this binary does not open, with the fix the caller chooses:
/// `what` says how the store at `store_dir` tells it was written by another build.
pub(crate) fn other_format(store_dir: &Path, what: &str, fix: &str) -> StoreError {
    StoreError::Format {
        message: format!(
            "this store was written by a different rdy build: {} {what}, and this rdy uses store \
             format {STORE_FORMAT}",
            store_dir.display()
        ),
        fix: fix.into(),
    }
}

/// The refusal for a store with no format number to go by (the fix is to re-create it).
pub(crate) fn other_build(store_dir: &Path, what: &str) -> StoreError {
    other_format(store_dir, what, RE_CREATE)
}

/// The refusal for a store path where there is nothing: `rdy init` creates a store, and opening
/// never does.
pub(crate) fn no_store(store_dir: &Path) -> StoreError {
    StoreError::Format {
        message: format!(
            "there is no store at {}: rdy init creates one and opening never does, and this rdy \
             uses store format {STORE_FORMAT}",
            store_dir.display()
        ),
        fix: RE_CREATE.into(),
    }
}

/// Reads `<store_dir>/FORMAT`: None if the file does not exist. A file with no `store_format`
/// line is another build's store and is refused.
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
        fix: "restore FORMAT from backup, or re-create it with this rdy".into(),
    };
    let text = String::from_utf8(bytes).map_err(|_| unreadable("it is not UTF-8 text".into()))?;
    match Format::parse(&text).map_err(unreadable)? {
        Some(f) => Ok(Some(f)),
        None => Err(other_build(
            store_dir,
            "has a FORMAT file with no store_format line",
        )),
    }
}

/// Writes `<store_dir>/FORMAT` durably.
pub fn write_format_file(store_dir: &Path, f: Format) -> Result<(), StoreError> {
    fsutil::write_durable(&store_dir.join(FORMAT_FILE), f.to_text().as_bytes())?;
    Ok(())
}

/// Refuses a store of any format but this binary's.
pub fn check_format(f: Format, store_dir: &Path) -> Result<(), StoreError> {
    if f.store_format != STORE_FORMAT {
        return Err(other_format(
            store_dir,
            &format!("has store format {}", f.store_format),
            &fix_for(f.store_format, STORE_FORMAT),
        ));
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
