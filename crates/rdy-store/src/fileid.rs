//! A file's identity on its filesystem (FW-XP1 D-1RXEA): on Unix, `st_dev` and `st_ino`.
//!
//! One type and one read serve the store lock, the working copy and the daemon. The project's
//! identity is the `FileId` of its `.rdy` directory (D-R7N9J).

use std::fmt;
use std::fs::File;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Written `{dev}:{ino}`, two decimal numbers: the socket record and the hello carry this text
/// (D-R7N9J). Ordered by `dev`, then `ino`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId {
    pub dev: u64,
    pub ino: u64,
}

impl FileId {
    /// The identity in `meta`: what `lstat` or `fstat` read, whichever made it.
    pub fn of(meta: &std::fs::Metadata) -> FileId {
        FileId {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }

    /// From `lstat`: a link is never followed.
    pub fn lstat(path: &Path) -> io::Result<FileId> {
        std::fs::symlink_metadata(path).map(|m| FileId::of(&m))
    }

    /// From `fstat` of an open file.
    pub fn of_file(f: &File) -> io::Result<FileId> {
        f.metadata().map(|m| FileId::of(&m))
    }
}

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.dev, self.ino)
    }
}

impl std::str::FromStr for FileId {
    type Err = String;

    /// `{dev}:{ino}`, two decimal numbers.
    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || format!("{s:?} is not a project identity (dev:ino)");
        let (dev, ino) = s.split_once(':').ok_or_else(bad)?;
        let number = |n: &str| {
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad());
            }
            n.parse::<u64>().map_err(|_| bad())
        };
        Ok(FileId {
            dev: number(dev)?,
            ino: number(ino)?,
        })
    }
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

/// Whether `a` and `b` are one file: both `lstat` and give one `FileId`.
pub fn same_file(a: &Path, b: &Path) -> bool {
    matches!((FileId::lstat(a), FileId::lstat(b)), (Ok(x), Ok(y)) if x == y)
}

/// What `FileStamp` reads besides length and mtime: the inode and the change time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeStamp {
    pub ino: u64,
    pub ctime: std::time::SystemTime,
}

/// The inode and the change time in `meta`, at full precision. A `ctime` that does not fit is
/// `UNIX_EPOCH`.
pub fn change_stamp(meta: &std::fs::Metadata) -> ChangeStamp {
    let ctime = u64::try_from(meta.ctime()).map_or(std::time::UNIX_EPOCH, |s| {
        std::time::UNIX_EPOCH + std::time::Duration::new(s, meta.ctime_nsec() as u32)
    });
    ChangeStamp {
        ino: meta.ino(),
        ctime,
    }
}

#[cfg(not(unix))]
compile_error!("rdy_store::fileid has no implementation for this platform yet (FW-XP1 Phase 4)");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps2_the_text_form_is_dev_colon_ino_and_parses_back() {
        for id in [
            FileId { dev: 1, ino: 2 },
            FileId { dev: 0, ino: 0 },
            FileId {
                dev: u64::MAX,
                ino: u64::MAX,
            },
        ] {
            let text = id.to_string();
            assert_eq!(text, format!("{}:{}", id.dev, id.ino));
            assert_eq!(text.parse::<FileId>(), Ok(id));
        }
        assert_eq!(
            FileId {
                dev: 16777232,
                ino: 42
            }
            .to_string(),
            "16777232:42"
        );
        assert!(FileId { dev: 1, ino: 9 } < FileId { dev: 2, ino: 0 });
        assert!(FileId { dev: 1, ino: 1 } < FileId { dev: 1, ino: 2 });
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
                Err(format!("{s:?} is not a project identity (dev:ino)")),
                "{s:?}"
            );
        }
    }

    #[test]
    fn ps2_serde_writes_the_text_form() {
        let id = FileId { dev: 7, ino: 11 };
        assert_eq!(serde_json::to_string(&id).unwrap(), r#""7:11""#);
        assert_eq!(serde_json::from_str::<FileId>(r#""7:11""#).unwrap(), id);
        let err = serde_json::from_str::<FileId>(r#""7""#).unwrap_err();
        assert!(
            err.to_string()
                .contains(r#""7" is not a project identity (dev:ino)"#),
            "{err}"
        );
        assert!(serde_json::from_str::<FileId>("[7,11]").is_err());
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
            FileId {
                dev: link_meta.dev(),
                ino: link_meta.ino()
            }
        );
        assert_eq!(FileId::of(&link_meta), of_link);
        let target_meta = std::fs::metadata(&target).unwrap();
        assert_eq!(
            of_target,
            FileId {
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
        let stamp = change_stamp(&meta);
        assert_eq!(stamp.ino, meta.ino());
        assert_eq!(stamp.ino, FileId::of(&meta).ino);
        let ctime = std::time::UNIX_EPOCH
            + std::time::Duration::new(
                u64::try_from(meta.ctime()).unwrap(),
                meta.ctime_nsec() as u32,
            );
        assert_eq!(stamp.ctime, ctime);
        assert!(stamp.ctime > std::time::UNIX_EPOCH);
        assert_eq!(change_stamp(&meta), stamp, "one stat gives one stamp");
    }
}
