//! A file's identity on its filesystem (FW-XP1 D-1RXEA), read from the object a name or a
//! handle reaches: on Unix `st_dev` and `st_ino`, on Windows the volume serial number and the
//! 128-bit file ID of `FILE_ID_INFO`.
//!
//! One type and one read serve the store lock, the working copy and the daemon. The project's
//! identity is the `FileId` of its `.rdy` directory (D-R7N9J). An identity is never part of a
//! stored object's encoding: it lives in run state, caches and checks only.

use std::fmt;
use std::fs::File;
use std::io;
use std::path::Path;

/// One file's identity, tagged with the platform that read it. Ordered by platform, then by
/// its fields in order.
///
/// - Unix: written `{dev}:{ino}`, two decimal numbers. The socket record and the hello carry
///   this text (D-R7N9J).
/// - Windows: written `w:{volume}:{id}`: the volume serial number as 16 lowercase hex digits,
///   then the 16 bytes of `FILE_ID_128.Identifier` from index 0 through 15, two lowercase hex
///   digits each (never reordered as GUID fields). The whole text is [`WINDOWS_TEXT_LEN`]
///   bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileId {
    Unix { dev: u64, ino: u64 },
    Windows { volume: u64, id: [u8; 16] },
}

/// The length of a Windows identity's text: `w:`, 16 digits, `:`, 32 digits.
pub const WINDOWS_TEXT_LEN: usize = 2 + 16 + 1 + 32;

impl FileId {
    /// The identity of what `path` names, a link never followed: `lstat` on Unix; on Windows an
    /// open of the name itself (a reparse point is not followed) and its `FILE_ID_INFO`. A
    /// failed read is an error, `could not read the file identity of {path}: {error}`, of the
    /// same kind as the failed call's.
    pub fn lstat(path: &Path) -> io::Result<FileId> {
        os::id_at(path).map_err(|e| unidentified(path, e))
    }

    /// The identity of the open file `f`, read from its handle. The error is the failed call's.
    pub fn of_file(f: &File) -> io::Result<FileId> {
        os::id_of(f)
    }

    /// The identity of the open file `f`, read from its handle. A failed read is an error,
    /// `could not read the file identity of {path}: {error}`, of the same kind as the failed
    /// call's.
    pub fn of_open(f: &File, path: &Path) -> io::Result<FileId> {
        os::id_of(f).map_err(|e| unidentified(path, e))
    }
}

/// `could not read the file identity of {path}: {error}`, of the same kind as `e`.
fn unidentified(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(
        e.kind(),
        format!(
            "could not read the file identity of {}: {e}",
            path.display()
        ),
    )
}

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileId::Unix { dev, ino } => write!(f, "{dev}:{ino}"),
            FileId::Windows { volume, id } => {
                write!(f, "w:{volume:016x}:")?;
                id.iter().try_for_each(|b| write!(f, "{b:02x}"))
            }
        }
    }
}

impl std::str::FromStr for FileId {
    type Err = String;

    /// `{dev}:{ino}`, two decimal numbers, or `w:{volume}:{id}`, exactly 16 and 32 lowercase
    /// hex digits. A Windows text's length is checked before anything else is read.
    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || format!("{s:?} is not a project identity (dev:ino or w:<volume>:<file id>)");
        if let Some(rest) = s.strip_prefix("w:") {
            return windows_text(s, rest).ok_or_else(bad);
        }
        let (dev, ino) = s.split_once(':').ok_or_else(bad)?;
        let number = |n: &str| {
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad());
            }
            n.parse::<u64>().map_err(|_| bad())
        };
        Ok(FileId::Unix {
            dev: number(dev)?,
            ino: number(ino)?,
        })
    }
}

/// The Windows identity `s` writes, `rest` being what follows its `w:`; none unless it is
/// exactly [`WINDOWS_TEXT_LEN`] bytes of the one shape.
fn windows_text(s: &str, rest: &str) -> Option<FileId> {
    if s.len() != WINDOWS_TEXT_LEN {
        return None;
    }
    let rest = rest.as_bytes();
    let (volume, id) = (&rest[..16], &rest[16..]);
    let id = id.strip_prefix(b":")?;
    let digit = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let mut v = 0u64;
    for &b in volume {
        v = (v << 4) | u64::from(digit(b)?);
    }
    let mut bytes = [0u8; 16];
    let (pairs, _) = id.as_chunks::<2>();
    for (out, [hi, lo]) in bytes.iter_mut().zip(pairs) {
        *out = (digit(*hi)? << 4) | digit(*lo)?;
    }
    Some(FileId::Windows {
        volume: v,
        id: bytes,
    })
}

impl serde::Serialize for FileId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for FileId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// Whether `a` and `b` are one file: both names reach one identity, no link followed.
pub fn same_file(a: &Path, b: &Path) -> bool {
    matches!((FileId::lstat(a), FileId::lstat(b)), (Ok(x), Ok(y)) if x == y)
}

/// What `FileStamp` reads besides length and mtime: the full identity and the change time. A
/// change stamp is a cache hint, never proof that the bytes stayed the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeStamp {
    pub id: FileId,
    pub ctime: std::time::SystemTime,
}

/// The change stamp of the open file `f`, read from its handle: its identity and its change
/// time at full precision (`st_ctime` on Unix, a `ctime` that does not fit being `UNIX_EPOCH`;
/// `FILE_BASIC_INFO.ChangeTime` on Windows). A failed read is an error,
/// `could not read change stamp for {path}: {error}`.
pub fn change_stamp_of(f: &File, path: &Path) -> io::Result<ChangeStamp> {
    let id = os::id_of(f).map_err(|e| unread("change stamp", path, e))?;
    let ctime = os::change_time(f).map_err(|e| unread("change stamp", path, e))?;
    Ok(ChangeStamp { id, ctime })
}

/// How many links (names) the open file `f` has, read from its handle. A failed read is an
/// error, `could not read link count for {path}: {error}`.
pub fn links_of(f: &File, path: &Path) -> io::Result<u64> {
    os::links(f).map_err(|e| unread("link count", path, e))
}

/// `could not read {field} for {path}: {error}`, of the same kind as `e`.
fn unread(field: &str, path: &Path, e: io::Error) -> io::Error {
    io::Error::new(
        e.kind(),
        format!("could not read {field} for {}: {e}", path.display()),
    )
}

/// The platform fields this module reads. Only this module names a platform's metadata.
#[cfg(unix)]
mod os {
    use super::FileId;
    use std::fs::File;
    use std::io;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    fn id(meta: &std::fs::Metadata) -> FileId {
        FileId::Unix {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }

    /// `st_dev` and `st_ino` from `lstat`.
    pub(super) fn id_at(path: &Path) -> io::Result<FileId> {
        std::fs::symlink_metadata(path).map(|m| id(&m))
    }

    /// `st_dev` and `st_ino` from `fstat`.
    pub(super) fn id_of(f: &File) -> io::Result<FileId> {
        f.metadata().map(|m| id(&m))
    }

    /// `st_ctime` at full precision from `fstat`; one that does not fit is `UNIX_EPOCH`.
    pub(super) fn change_time(f: &File) -> io::Result<std::time::SystemTime> {
        let meta = f.metadata()?;
        Ok(
            u64::try_from(meta.ctime()).map_or(std::time::UNIX_EPOCH, |s| {
                std::time::UNIX_EPOCH + std::time::Duration::new(s, meta.ctime_nsec() as u32)
            }),
        )
    }

    /// `st_nlink` from `fstat`.
    pub(super) fn links(f: &File) -> io::Result<u64> {
        f.metadata().map(|m| m.nlink())
    }
}

/// The handle queries on Windows: `FILE_ID_INFO`, `FILE_BASIC_INFO` and `FILE_STANDARD_INFO`.
#[cfg(windows)]
mod windows;

#[cfg(windows)]
mod os {
    use super::FileId;
    use super::windows as w;
    use std::fs::File;
    use std::io;
    use std::path::Path;

    pub(super) fn id_at(path: &Path) -> io::Result<FileId> {
        id_of(&w::open_no_follow(path)?)
    }

    pub(super) fn id_of(f: &File) -> io::Result<FileId> {
        let (volume, id) = w::id_of(f)?;
        Ok(FileId::Windows { volume, id })
    }

    pub(super) fn change_time(f: &File) -> io::Result<std::time::SystemTime> {
        w::change_time(f)
    }

    pub(super) fn links(f: &File) -> io::Result<u64> {
        w::links(f)
    }
}

#[cfg(not(any(unix, windows)))]
compile_error!("rdy_store::fileid has no implementation for this platform (FW-XP1 D-1RXEA)");

/// Unix only: the tests read `st_dev`, `st_ino` and `st_ctime` and make symbolic links.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn unix(meta: &std::fs::Metadata) -> FileId {
        FileId::Unix {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }

    #[test]
    fn ps2_lstat_never_follows_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        std::fs::write(&target, b"x").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let of_target = FileId::lstat(&target).unwrap();
        let of_link = FileId::lstat(&link).unwrap();
        assert_ne!(of_link, of_target, "the link is its own file");
        let link_meta = std::fs::symlink_metadata(&link).unwrap();
        assert_eq!(
            of_link,
            FileId::Unix {
                dev: link_meta.dev(),
                ino: link_meta.ino()
            }
        );
        assert_eq!(unix(&link_meta), of_link);
        let target_meta = std::fs::metadata(&target).unwrap();
        assert_eq!(
            of_target,
            FileId::Unix {
                dev: target_meta.dev(),
                ino: target_meta.ino()
            }
        );
        assert_eq!(
            FileId::of_file(&std::fs::File::open(&target).unwrap()).unwrap(),
            of_target,
            "fstat of the open file gives the identity lstat gives"
        );

        let dangling = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("missing"), &dangling).unwrap();
        assert!(
            FileId::lstat(&dangling).is_ok(),
            "a dangling link has an identity"
        );
        assert_eq!(
            FileId::lstat(&dir.path().join("missing"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[test]
    fn ps2_a_hard_link_is_the_same_file_and_a_copy_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        let hard = dir.path().join("hard");
        let copy = dir.path().join("copy");
        let soft = dir.path().join("soft");
        std::fs::write(&file, b"x").unwrap();
        std::fs::hard_link(&file, &hard).unwrap();
        std::fs::copy(&file, &copy).unwrap();
        std::os::unix::fs::symlink(&file, &soft).unwrap();

        assert!(same_file(&file, &file));
        assert!(same_file(&file, &hard));
        assert!(same_file(&hard, &file));
        assert!(!same_file(&file, &copy));
        assert!(!same_file(&file, &soft), "a symbolic link is not followed");
        assert!(!same_file(&file, &dir.path().join("missing")));
        assert!(!same_file(
            &dir.path().join("missing"),
            &dir.path().join("missing")
        ));
    }

    #[test]
    fn ps2_change_stamp_reads_the_inode_and_ctime() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        let meta = std::fs::symlink_metadata(&file).unwrap();
        let handle = std::fs::File::open(&file).unwrap();
        let stamp = change_stamp_of(&handle, &file).unwrap();
        assert_eq!(stamp.id, unix(&meta));
        assert_eq!(stamp.id, FileId::lstat(&file).unwrap());
        let ctime = std::time::UNIX_EPOCH
            + std::time::Duration::new(
                u64::try_from(meta.ctime()).unwrap(),
                meta.ctime_nsec() as u32,
            );
        assert_eq!(stamp.ctime, ctime);
        assert!(stamp.ctime > std::time::UNIX_EPOCH);
        assert_eq!(
            change_stamp_of(&handle, &file).unwrap(),
            stamp,
            "one file gives one stamp"
        );
        assert_eq!(links_of(&handle, &file).unwrap(), meta.nlink());
    }
}

/// The text forms of both platforms' identities: pure parsing, so these run everywhere.
#[cfg(test)]
mod wx2_tests {
    use super::*;

    /// Two Windows identities on one volume whose 128-bit IDs share their low 64 bits (bytes
    /// 0 through 7) and differ only above them.
    fn same_low_half() -> (FileId, FileId) {
        let low = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77];
        let mut a = [0u8; 16];
        let mut b = [0u8; 16];
        a[..8].copy_from_slice(&low);
        b[..8].copy_from_slice(&low);
        a[8..].copy_from_slice(&[0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        b[8..].copy_from_slice(&[0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xfe]);
        (
            FileId::Windows {
                volume: 0x0123_4567_89ab_cdef,
                id: a,
            },
            FileId::Windows {
                volume: 0x0123_4567_89ab_cdef,
                id: b,
            },
        )
    }

    #[test]
    fn wx2_full_file_id_round_trips_without_truncation() {
        let (a, b) = same_low_half();
        assert_ne!(a, b, "the high 64 bits tell the files apart");
        assert_eq!(
            a.to_string(),
            "w:0123456789abcdef:00112233445566778899aabbccddeeff"
        );
        assert_eq!(
            b.to_string(),
            "w:0123456789abcdef:00112233445566778899aabbccddeefe"
        );
        assert!(b < a);
        for id in [
            a,
            b,
            FileId::Windows {
                volume: 0,
                id: [0; 16],
            },
            FileId::Windows {
                volume: u64::MAX,
                id: [0xff; 16],
            },
            FileId::Windows {
                volume: 7,
                id: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            },
        ] {
            let text = id.to_string();
            assert_eq!(text.len(), WINDOWS_TEXT_LEN, "{text}");
            assert_eq!(text.parse::<FileId>(), Ok(id), "{text}");
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("{text:?}"));
            assert_eq!(serde_json::from_str::<FileId>(&json).unwrap(), id);
        }
        // the bytes are written in index order, never as GUID fields
        assert_eq!(
            FileId::Windows {
                volume: 7,
                id: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            }
            .to_string(),
            "w:0000000000000007:0102030405060708090a0b0c0d0e0f10"
        );
        // the Unix text is unchanged, and the two platforms' identities never meet
        let unix = FileId::Unix { dev: 1, ino: 2 };
        assert_eq!(unix.to_string(), "1:2");
        assert_eq!("1:2".parse::<FileId>(), Ok(unix));
        assert_ne!(unix, a);

        // malformed Windows text is refused with the existing message, its length checked first
        let good = a.to_string();
        let long = format!("w:{}", "0".repeat(10_000));
        let upper = good.to_uppercase().replacen('W', "w", 1);
        let cases = [
            "w:".to_string(),
            "w::".to_string(),
            "W:0123456789abcdef:00112233445566778899aabbccddeeff".to_string(),
            upper,
            format!("{good}0"),
            good[..good.len() - 1].to_string(),
            "w:0123456789abcde:000112233445566778899aabbccddeeff".to_string(),
            "w:0123456789abcdef0:0112233445566778899aabbccddeeff".to_string(),
            "w:0123456789abcdef-00112233445566778899aabbccddeeff".to_string(),
            "w:0123456789abcdeg:00112233445566778899aabbccddeeff".to_string(),
            "w:0123456789abcdef:00112233445566778899aabbccddeefg".to_string(),
            "w:+123456789abcdef:00112233445566778899aabbccddeeff".to_string(),
            "w:0123456789abcdef:0011223344556677:8899aabbccddeef".to_string(),
            "x:0123456789abcdef:00112233445566778899aabbccddeeff".to_string(),
            "w:0123456789abcdé:00112233445566778899aabbccddeeff".to_string(),
            long,
        ];
        for s in cases {
            assert_eq!(
                s.parse::<FileId>(),
                Err(format!(
                    "{s:?} is not a project identity (dev:ino or w:<volume>:<file id>)"
                )),
                "{s:?}"
            );
        }
    }

    #[test]
    fn ps2_the_text_form_is_dev_colon_ino_and_parses_back() {
        for id in [
            FileId::Unix { dev: 1, ino: 2 },
            FileId::Unix { dev: 0, ino: 0 },
            FileId::Unix {
                dev: u64::MAX,
                ino: u64::MAX,
            },
        ] {
            let FileId::Unix { dev, ino } = id else {
                unreachable!()
            };
            let text = id.to_string();
            assert_eq!(text, format!("{dev}:{ino}"));
            assert_eq!(text.parse::<FileId>(), Ok(id));
        }
        assert_eq!(
            FileId::Unix {
                dev: 16777232,
                ino: 42
            }
            .to_string(),
            "16777232:42"
        );
        assert!(FileId::Unix { dev: 1, ino: 9 } < FileId::Unix { dev: 2, ino: 0 });
        assert!(FileId::Unix { dev: 1, ino: 1 } < FileId::Unix { dev: 1, ino: 2 });
    }

    #[test]
    fn ps2_a_malformed_identity_is_refused_with_its_text() {
        for s in [
            "",
            "1",
            "1:",
            ":2",
            "a:2",
            "1:2:3",
            "-1:2",
            "18446744073709551616:1",
        ] {
            assert_eq!(
                s.parse::<FileId>(),
                Err(format!(
                    "{s:?} is not a project identity (dev:ino or w:<volume>:<file id>)"
                )),
                "{s:?}"
            );
        }
    }

    #[test]
    fn ps2_serde_writes_the_text_form() {
        let id = FileId::Unix { dev: 7, ino: 11 };
        assert_eq!(serde_json::to_string(&id).unwrap(), r#""7:11""#);
        assert_eq!(serde_json::from_str::<FileId>(r#""7:11""#).unwrap(), id);
        let err = serde_json::from_str::<FileId>(r#""7""#).unwrap_err();
        assert!(
            err.to_string()
                .contains(r#""7" is not a project identity (dev:ino or w:<volume>:<file id>)"#),
            "{err}"
        );
        assert!(serde_json::from_str::<FileId>("[7,11]").is_err());
    }
}
