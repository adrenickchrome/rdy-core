//! Unix modes as a fixture on every host (FW-XP1 D-1RXEA). On Unix these are the file's own mode
//! bits. On Windows a mode stands for a DACL: the owner's bits are the owning account's ACE, the
//! group's are `Power Users` (another account, not every account), and the others' are `Everyone`.
//! The sticky bit is `Everyone` without the right to remove an entry. 0o600 on a file and 0o700 on
//! a directory are exactly what rdy makes private. [`mode`] reads the mode back from the ACL.

use rdy_store::pio;
use std::fs;
use std::io;
use std::path::Path;

/// A fresh directory that group and others cannot write, whatever the umask: `tempdir()` asks for
/// 0o777, so a umask of 002 (the Linux default for user private groups) would make it a
/// shared-writable home, which `LockedConfig` refuses. 0o700 minus any umask has no such bit.
pub fn home_dir() -> tempfile::TempDir {
    #[cfg(unix)]
    return tempfile::Builder::new()
        .permissions(pio::permissions(0o700))
        .tempdir()
        .unwrap();
    #[cfg(windows)]
    {
        let dir = tempfile::tempdir().unwrap();
        pio::make_private_dir(dir.path()).unwrap();
        dir
    }
}

/// Makes the directory `path` 0o700 minus the umask, so a fixture directory is never
/// group-writable under a umask of 002; a test that needs a wider mode sets it with [`chmod`].
pub fn mkdir(path: impl AsRef<Path>) -> io::Result<()> {
    #[cfg(unix)]
    return pio::dir_mode(&mut fs::DirBuilder::new(), 0o700).create(path);
    #[cfg(windows)]
    pio::create_private_dir(path.as_ref())
}

/// A symbolic link at `link` to `target`: on Windows a directory link when `target` is a
/// directory, else a file link.
pub fn link(target: impl AsRef<Path>, link: impl AsRef<Path>) -> io::Result<()> {
    #[cfg(unix)]
    return pio::symlink(target, link);
    #[cfg(windows)]
    if target.as_ref().is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

/// What tells one file or directory at `path` from another; a link is not followed.
#[cfg(unix)]
pub fn identity(path: &Path) -> u64 {
    pio::inode(&fs::symlink_metadata(path).unwrap())
}

/// What tells one file or directory at `path` from another; a link is not followed.
#[cfg(windows)]
pub fn identity(path: &Path) -> rdy_store::fileid::FileId {
    rdy_store::fileid::FileId::of_open(&host::open_entry(path), path).unwrap()
}

/// The mode of `path`; a link is not followed.
#[cfg(unix)]
pub fn mode(path: &Path) -> u32 {
    pio::mode_of(&fs::symlink_metadata(path).unwrap())
}

/// Sets the mode of `path`.
#[cfg(unix)]
pub fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, pio::permissions(mode)).unwrap();
}

/// Opens `path` for writing, truncated and made if missing; a new file is 0o600 less the umask.
pub fn open_secret(path: &Path) -> fs::File {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    pio::file_mode(&mut opts, 0o600);
    opts.open(path).unwrap()
}

#[cfg(windows)]
pub use host::{chmod, mode};

#[cfg(windows)]
mod host {
    use rdy_store::pio;
    use std::fs::{self, File};
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::Path;
    use std::process::Command;

    const READ_CONTROL: u32 = 0x0002_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_READ_DATA: u32 = 0x1;
    const FILE_WRITE_DATA: u32 = 0x2;
    const FILE_EXECUTE: u32 = 0x20;
    const FILE_DELETE_CHILD: u32 = 0x40;
    const INHERIT_ONLY_ACE: u8 = 0x8;
    const POWER_USERS: &str = "S-1-5-32-547";
    const EVERYONE: &str = "S-1-1-0";

    /// `path` opened for its identity and security alone, with no link followed.
    pub fn open_entry(path: &Path) -> File {
        fs::OpenOptions::new()
            .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
            .share_mode(7)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap()
    }

    /// The mode the DACL of `path` stands for. A file has no execute bit.
    pub fn mode(path: &Path) -> u32 {
        let f = open_entry(path);
        let dir = f.metadata().unwrap().is_dir();
        let sec = pio::security_of(&f).unwrap();
        let owner = sec.owner.unwrap().to_string();
        let mut mode = 0;
        for ace in sec.dacl.unwrap_or_default() {
            let Some(sid) = ace.sid else { continue };
            if ace.kind != 0 || ace.flags & INHERIT_ONLY_ACE != 0 {
                continue;
            }
            let sid = sid.to_string();
            let shift = if sid == owner {
                6
            } else if sid == POWER_USERS {
                3
            } else if sid == EVERYONE {
                0
            } else {
                continue;
            };
            let mut bits = 0;
            if ace.mask & FILE_READ_DATA != 0 {
                bits |= 4;
            }
            if ace.mask & FILE_WRITE_DATA != 0 {
                bits |= 2;
            }
            if dir && ace.mask & FILE_EXECUTE != 0 {
                bits |= 1;
            }
            mode |= bits << shift;
            if dir && shift == 0 && bits & 2 != 0 && ace.mask & FILE_DELETE_CHILD == 0 {
                mode |= 0o1000;
            }
        }
        mode
    }

    /// The icacls rights of one class's mode `bits`.
    fn rights(bits: u32, sticky: bool) -> &'static str {
        match (bits, sticky) {
            (7, true) => "(RX,W)",
            (7, false) | (6, false) => "(F)",
            (6, true) => "(R,W)",
            (5, _) => "(RX)",
            (4, _) => "(R)",
            (3, _) => "(W,X)",
            (2, _) => "(W)",
            (1, _) => "(X)",
            _ => unreachable!("a class with no bits has no ACE"),
        }
    }

    fn icacls(path: &Path, args: &[String]) {
        let out = Command::new("icacls")
            .arg(path)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "icacls {args:?}: {out:?}");
    }

    /// Sets the DACL that `mode` stands for on `path`.
    pub fn chmod(path: &Path, mode: u32) {
        let f = open_entry(path);
        let dir = f.metadata().unwrap().is_dir();
        if !dir && mode == 0o600 {
            pio::make_private_file(&f).unwrap();
            return;
        }
        if dir && mode == 0o700 {
            pio::make_private_dir(path).unwrap();
            return;
        }
        let owner = pio::security_of(&f).unwrap().owner.unwrap().to_string();
        drop(f);
        let sticky = mode & 0o1000 != 0;
        let mut args = vec!["/inheritance:r".to_string()];
        let classes = [
            (&owner, (mode >> 6) & 7, false),
            (&POWER_USERS.to_string(), (mode >> 3) & 7, false),
            (&EVERYONE.to_string(), mode & 7, sticky),
        ]
        .map(|(sid, bits, sticky)| (sid.clone(), bits, sticky));
        args.push("/grant:r".to_string());
        for (sid, bits, sticky) in &classes {
            if *bits != 0 {
                args.push(format!("*{sid}:{}", rights(*bits, *sticky)));
            }
        }
        icacls(path, &args);
        // a class with no bits holds no ACE, whatever stood before
        for (sid, bits, _) in &classes {
            if *bits == 0 {
                icacls(path, &["/remove".to_string(), format!("*{sid}")]);
            }
        }
    }
}
