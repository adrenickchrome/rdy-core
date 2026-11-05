//! The Windows privacy and no-follow calls of [`crate::pio`] (FW-XP1 Phase 4).
//!
//! A private object is owned by this account, and its DACL is protected (it inherits nothing)
//! and holds one allow ACE: this account, with every right (`FILE_ALL_ACCESS`). A directory's
//! ACE is also inherited by what is made in it (`OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE`).
//! - A new private file or directory gets that descriptor when it is made, never later: there is
//!   no moment at which another account may open it.
//! - An existing object is made private only when this account owns it. One that another account
//!   owns is refused and left as it is, never repaired, even when its DACL would let this account
//!   change it.
//! - A reparse point of any tag (a symbolic link, a junction or other mount point, or any other
//!   tag) is opened itself, never followed, and refused.
//! - Every check reads the object that was opened, through its handle, never a name again.
//!
//! A security descriptor is read whole into a buffer of at most [`DESCRIPTOR_MAX_BYTES`], and
//! every offset, size and count in it is checked before it is used. A SID is kept in
//! [`SID_MAX_BYTES`], the OS maximum, and its length is checked before it is copied. A malformed
//! descriptor, ACL or SID is the OS error of that kind (`ERROR_INVALID_SECURITY_DESCR`,
//! `ERROR_INVALID_ACL`, `ERROR_INVALID_SID`), never `InvalidData`, which diagnoses log damage.
//!
//! This file depends only on `std`, `windows-sys`, [`crate::fileid`] and [`Privacy`], so an
//! isolated probe can compile and test it with those files alone.

use std::ffi::{OsString, c_void};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{
    ERROR_FILENAME_EXCED_RANGE, ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_ACL,
    ERROR_INVALID_SECURITY_DESCR, ERROR_INVALID_SID, GENERIC_ALL, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{
    CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, GetKernelObjectSecurity, GetTokenInformation,
    INHERIT_ONLY_ACE, OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION, SE_DACL_PRESENT,
    SE_DACL_PROTECTED, SE_SELF_RELATIVE, SECURITY_ATTRIBUTES, SECURITY_MAX_SID_SIZE,
    SetKernelObjectSecurity, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, DELETE, FILE_ALL_ACCESS, FILE_APPEND_DATA,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
    FILE_DELETE_CHILD, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_WRITE_DATA, FileAttributeTagInfo, GetFileInformationByHandleEx,
    GetFinalPathNameByHandleW, READ_CONTROL, ReOpenFile, SYNCHRONIZE, VOLUME_NAME_DOS, WRITE_DAC,
    WRITE_OWNER,
};
use windows_sys::Win32::System::SystemServices::{
    ACCESS_ALLOWED_ACE_TYPE, ACCESS_ALLOWED_CALLBACK_ACE_TYPE, ACCESS_DENIED_ACE_TYPE,
    ACCESS_DENIED_CALLBACK_ACE_TYPE, ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE,
    ACCESS_DENIED_OBJECT_ACE_TYPE, SECURITY_DESCRIPTOR_REVISION,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::fileid::FileId;
use crate::pio::Privacy;

/// The largest SID: a revision, a count, a 6-byte authority and 15 sub-authorities.
pub const SID_MAX_BYTES: usize = 68;
const _: () = assert!(SID_MAX_BYTES == SECURITY_MAX_SID_SIZE as usize);

/// The largest security descriptor that is read: the 20-byte self-relative header, an owner SID
/// of at most [`SID_MAX_BYTES`] and an ACL of at most 65,535 bytes (its size is 16 bits),
/// rounded up to 4 bytes. Only the owner and the DACL are asked for.
pub const DESCRIPTOR_MAX_BYTES: usize = 65_624;

/// The longest native path, in UTF-16 code units.
const NATIVE_PATH_MAX: usize = 32_767;

/// A self-relative descriptor's header: revision, padding, control, then the offsets of the
/// owner, the group, the SACL and the DACL.
const HEADER_BYTES: usize = 20;
/// An ACL's header: revision, padding, size, ACE count, padding.
const ACL_HEADER_BYTES: usize = 8;
/// The smallest ACE that rdy reads a SID from: type, flags, size, mask.
const ACE_FIXED_BYTES: usize = 8;

/// The ACE flags of a private directory's one ACE: what is made in it inherits that ACE.
const DIR_ACE_FLAGS: u8 = (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8;

/// Rights that let an account change an object, its descriptor or, in a directory, its entries:
/// another account that holds one may write the object ([`Privacy::shared_writable`]).
const WRITE_RIGHTS: u32 = FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | FILE_DELETE_CHILD
    | DELETE
    | WRITE_DAC
    | WRITE_OWNER
    | GENERIC_WRITE
    | GENERIC_ALL;

/// Rights that let an account remove or replace an entry of a directory that it does not own:
/// all accounts holding one open the directory to all ([`Privacy::open_to_all`]). Adding an entry
/// alone does not: it is the sticky directory of Unix.
const REPLACE_ENTRY_RIGHTS: u32 = FILE_DELETE_CHILD | WRITE_DAC | WRITE_OWNER | GENERIC_ALL;

/// A SID, kept in [`SID_MAX_BYTES`]. Its text is the standard string form, `S-1-5-21-…`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sid {
    len: u8,
    bytes: [u8; SID_MAX_BYTES],
}

impl Sid {
    /// The SID at the start of `bytes`: revision 1, at most 15 sub-authorities, and every byte of
    /// it within `bytes`. Anything else is `ERROR_INVALID_SID`; nothing is copied before its
    /// length is checked.
    pub fn parse(bytes: &[u8]) -> io::Result<Sid> {
        let (&revision, &count) = match bytes {
            [revision, count, ..] => (revision, count),
            _ => return Err(os_error(ERROR_INVALID_SID)),
        };
        let len = 8 + 4 * usize::from(count);
        if revision != 1 || count > 15 || len > bytes.len() || len > SID_MAX_BYTES {
            return Err(os_error(ERROR_INVALID_SID));
        }
        let mut out = [0; SID_MAX_BYTES];
        out[..len].copy_from_slice(&bytes[..len]);
        Ok(Sid {
            len: len as u8,
            bytes: out,
        })
    }

    /// The SID with identifier authority `authority` (at most 2^32 - 1, as every well-known
    /// authority) and the sub-authorities `subs` (at most 15).
    fn of(authority: u32, subs: &[u32]) -> Sid {
        let mut b = vec![1, subs.len() as u8, 0, 0];
        b.extend(authority.to_be_bytes());
        for s in subs {
            b.extend(s.to_le_bytes());
        }
        Sid::parse(&b).expect("a well-known SID is valid")
    }

    /// The SID's bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

impl fmt::Display for Sid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.as_bytes();
        let authority = b[2..8].iter().fold(0u64, |a, x| (a << 8) | u64::from(*x));
        write!(f, "S-{}-", b[0])?;
        if authority >> 32 == 0 {
            write!(f, "{authority}")?;
        } else {
            write!(f, "0x{authority:012X}")?;
        }
        for sub in b[8..].as_chunks::<4>().0 {
            write!(f, "-{}", u32::from_le_bytes(*sub))?;
        }
        Ok(())
    }
}

impl fmt::Debug for Sid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// The kind of a private object: its one ACE is inherited by what is made in a directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    File,
    Dir,
}

/// One ACE of a DACL, as read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ace {
    /// The ACE type (`ACCESS_ALLOWED_ACE_TYPE`, `ACCESS_DENIED_ACE_TYPE`, ...).
    pub kind: u8,
    /// The ACE flags (`OBJECT_INHERIT_ACE`, `INHERITED_ACE`, ...).
    pub flags: u8,
    /// The access mask.
    pub mask: u32,
    /// The trustee, for the allowed, denied and callback types; None for the object types,
    /// whose SID follows object GUIDs, and for any other type.
    pub sid: Option<Sid>,
}

impl Ace {
    /// The one ACE of a private object of `kind`: `account` is allowed every right, and in a
    /// directory what is made there inherits it.
    pub fn private(account: Sid, kind: ObjectKind) -> Ace {
        Ace {
            kind: ACCESS_ALLOWED_ACE_TYPE as u8,
            flags: match kind {
                ObjectKind::File => 0,
                ObjectKind::Dir => DIR_ACE_FLAGS,
            },
            mask: FILE_ALL_ACCESS,
            sid: Some(account),
        }
    }
}

/// The owner and the DACL of an object, as read from its security descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Security {
    /// None when the descriptor has no owner.
    pub owner: Option<Sid>,
    /// The DACL is protected: it inherits no ACE from the parent.
    pub protected: bool,
    /// None when there is no DACL, or a null one: every account has every right.
    pub dacl: Option<Vec<Ace>>,
}

/// A self-relative security descriptor of a private object: owned by one account, with a
/// protected DACL that holds that account's one ACE ([`Ace::private`]). New files, directories
/// and other objects of this account get it when they are made.
pub struct PrivateDescriptor {
    words: Box<[u32]>,
}

impl PrivateDescriptor {
    /// The descriptor of a private object of `kind` that `account` owns.
    pub fn new(account: &Sid, kind: ObjectKind) -> PrivateDescriptor {
        let bytes = descriptor_bytes(account, &[Ace::private(*account, kind)]);
        PrivateDescriptor {
            words: bytes
                .chunks(4)
                .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
                .collect(),
        }
    }

    /// The descriptor of a private object of `kind` that this account owns.
    pub fn of_this_account(kind: ObjectKind) -> io::Result<PrivateDescriptor> {
        Ok(PrivateDescriptor::new(&current_account()?, kind))
    }

    /// The descriptor's bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// The descriptor, for a `PSECURITY_DESCRIPTOR` parameter that reads it, for as long as
    /// `self` lives.
    pub fn as_ptr(&self) -> *mut c_void {
        self.words.as_ptr() as *mut c_void
    }

    /// The `SECURITY_ATTRIBUTES` that make a new object with this descriptor, for as long as
    /// `self` lives. Its handle is not inherited.
    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.as_ptr(),
            bInheritHandle: 0,
        }
    }
}

/// A self-relative descriptor owned by `owner`, with a protected DACL that holds `aces` (allowed
/// and denied types, each with its SID). Every length is a multiple of 4.
fn descriptor_bytes(owner: &Sid, aces: &[Ace]) -> Vec<u8> {
    let owner = owner.as_bytes();
    let mut acl = Vec::new();
    for ace in aces {
        let sid = ace.sid.as_ref().map(Sid::as_bytes).unwrap_or_default();
        acl.push(ace.kind);
        acl.push(ace.flags);
        acl.extend(((ACE_FIXED_BYTES + sid.len()) as u16).to_le_bytes());
        acl.extend(ace.mask.to_le_bytes());
        acl.extend(sid);
    }
    let acl_size = (ACL_HEADER_BYTES + acl.len()) as u16;
    let dacl_at = (HEADER_BYTES + owner.len()) as u32;
    let control = SE_SELF_RELATIVE | SE_DACL_PRESENT | SE_DACL_PROTECTED;
    let mut sd = vec![SECURITY_DESCRIPTOR_REVISION as u8, 0];
    sd.extend(control.to_le_bytes());
    sd.extend((HEADER_BYTES as u32).to_le_bytes());
    sd.extend([0; 8]);
    sd.extend(dacl_at.to_le_bytes());
    sd.extend(owner);
    sd.extend([2, 0]);
    sd.extend(acl_size.to_le_bytes());
    sd.extend((aces.len() as u16).to_le_bytes());
    sd.extend([0, 0]);
    sd.extend(acl);
    sd
}

/// The owner and the DACL of the self-relative descriptor `sd`. Every offset, size and count is
/// checked against `sd` before it is used.
fn parse_security(sd: &[u8]) -> io::Result<Security> {
    let bad = || os_error(ERROR_INVALID_SECURITY_DESCR);
    if sd.len() < HEADER_BYTES || u32::from(sd[0]) != SECURITY_DESCRIPTOR_REVISION {
        return Err(bad());
    }
    let control = u16::from_le_bytes([sd[2], sd[3]]);
    if control & SE_SELF_RELATIVE == 0 {
        return Err(bad());
    }
    let offset = |at: usize| u32::from_le_bytes([sd[at], sd[at + 1], sd[at + 2], sd[at + 3]]);
    let part = |at: usize| match offset(at) {
        0 => Ok(None),
        o => usize::try_from(o)
            .ok()
            .and_then(|o| sd.get(o..))
            .filter(|rest| !rest.is_empty())
            .map(Some)
            .ok_or_else(bad),
    };
    let owner = part(4)?.map(Sid::parse).transpose()?;
    let dacl = match part(16)? {
        Some(acl) if control & SE_DACL_PRESENT != 0 => Some(parse_acl(acl)?),
        _ => None,
    };
    Ok(Security {
        owner,
        protected: control & SE_DACL_PROTECTED != 0,
        dacl,
    })
}

/// The ACEs of the ACL at the start of `acl`. Its size and every ACE must lie within `acl`, and
/// its count within the room its size leaves, before anything is allocated.
fn parse_acl(acl: &[u8]) -> io::Result<Vec<Ace>> {
    let bad = || os_error(ERROR_INVALID_ACL);
    if acl.len() < ACL_HEADER_BYTES || !(2..=4).contains(&acl[0]) {
        return Err(bad());
    }
    let size = usize::from(u16::from_le_bytes([acl[2], acl[3]]));
    let count = usize::from(u16::from_le_bytes([acl[4], acl[5]]));
    if size < ACL_HEADER_BYTES || size > acl.len() {
        return Err(bad());
    }
    let body = &acl[ACL_HEADER_BYTES..size];
    if count > body.len() / ACE_FIXED_BYTES {
        return Err(bad());
    }
    let mut aces = Vec::with_capacity(count);
    let mut at = 0;
    for _ in 0..count {
        let header = body.get(at..at + 4).ok_or_else(bad)?;
        let ace_size = usize::from(u16::from_le_bytes([header[2], header[3]]));
        if ace_size < ACE_FIXED_BYTES {
            return Err(bad());
        }
        let ace = body.get(at..at + ace_size).ok_or_else(bad)?;
        let kind = ace[0];
        let with_sid = [
            ACCESS_ALLOWED_ACE_TYPE,
            ACCESS_DENIED_ACE_TYPE,
            ACCESS_ALLOWED_CALLBACK_ACE_TYPE,
            ACCESS_DENIED_CALLBACK_ACE_TYPE,
        ]
        .contains(&u32::from(kind));
        aces.push(Ace {
            kind,
            flags: ace[1],
            mask: u32::from_le_bytes([ace[4], ace[5], ace[6], ace[7]]),
            sid: if with_sid {
                Some(Sid::parse(&ace[ACE_FIXED_BYTES..])?)
            } else {
                None
            },
        });
        at += ace_size;
    }
    Ok(aces)
}

/// The well-known SIDs whose access gives no account more than it already has: `SYSTEM`, and
/// `BUILTIN\Administrators`, whose members may take ownership of any object and read it through
/// their backup right whatever its DACL. They are the Unix superuser.
fn privileged() -> &'static [Sid; 2] {
    static SIDS: OnceLock<[Sid; 2]> = OnceLock::new();
    SIDS.get_or_init(|| [Sid::of(5, &[18]), Sid::of(5, &[32, 544])])
}

/// `OWNER RIGHTS`: an ACE for it applies to the object's owner. `CREATOR OWNER` and
/// `CREATOR GROUP`: placeholders that the access check never matches.
fn owner_sids() -> &'static [Sid; 3] {
    static SIDS: OnceLock<[Sid; 3]> = OnceLock::new();
    SIDS.get_or_init(|| [Sid::of(3, &[4]), Sid::of(3, &[0]), Sid::of(3, &[1])])
}

/// The well-known groups that hold every local account, or any account at all (closed list):
/// Everyone, LOCAL, CONSOLE LOGON, NETWORK, INTERACTIVE, ANONYMOUS LOGON, Authenticated Users,
/// This Organization, Local account, `BUILTIN\Users` and `BUILTIN\Guests`.
fn all_accounts() -> &'static [Sid; 11] {
    static SIDS: OnceLock<[Sid; 11]> = OnceLock::new();
    SIDS.get_or_init(|| {
        [
            Sid::of(1, &[0]),
            Sid::of(2, &[0]),
            Sid::of(2, &[1]),
            Sid::of(5, &[2]),
            Sid::of(5, &[4]),
            Sid::of(5, &[7]),
            Sid::of(5, &[11]),
            Sid::of(5, &[15]),
            Sid::of(5, &[113]),
            Sid::of(5, &[32, 545]),
            Sid::of(5, &[32, 546]),
        ]
    })
}

/// Who an ACE that applies to the object lets in.
#[derive(PartialEq)]
enum Trustee {
    /// This account, the owner when this account owns it, or an account with no more than it
    /// already has: no other account gets access.
    Nobody,
    /// Every account.
    All,
    /// Another account or group.
    Other,
}

/// Who `sid` names, on an object owned by `owner`, judged by this account `me`. None is an ACE
/// whose SID is not read: every account, to be safe.
fn trustee(sid: Option<&Sid>, owner: Option<&Sid>, me: &Sid) -> Trustee {
    let Some(sid) = sid else {
        return Trustee::All;
    };
    let [owner_rights, creator_owner, creator_group] = owner_sids();
    if sid == me || privileged().contains(sid) || sid == creator_owner || sid == creator_group {
        Trustee::Nobody
    } else if sid == owner_rights {
        if owner == Some(me) {
            Trustee::Nobody
        } else {
            Trustee::Other
        }
    } else if all_accounts().contains(sid) {
        Trustee::All
    } else {
        Trustee::Other
    }
}

/// The [`Privacy`] of an object whose descriptor says `sec`, of kind `kind` (`reparse` when it is
/// a reparse point, which is never private), judged by this account `me`.
fn judge(sec: &Security, kind: ObjectKind, reparse: bool, me: &Sid) -> Privacy {
    let private =
        !reparse && sec.protected && sec.dacl.as_deref() == Some(&[Ace::private(*me, kind)][..]);
    let mut p = Privacy {
        mine: sec.owner.as_ref() == Some(me),
        private,
        owner_only: true,
        shared_writable: false,
        open_to_all: false,
    };
    let Some(aces) = &sec.dacl else {
        p.owner_only = false;
        p.shared_writable = true;
        p.open_to_all = true;
        return p;
    };
    let open_rights = match kind {
        ObjectKind::Dir => REPLACE_ENTRY_RIGHTS,
        ObjectKind::File => WRITE_RIGHTS,
    };
    for ace in aces {
        // an inherit-only ACE applies only to what is made in a directory
        if u32::from(ace.flags) & INHERIT_ONLY_ACE != 0 {
            continue;
        }
        let ace_type = u32::from(ace.kind);
        let (sid, mask) = if ace_type == ACCESS_ALLOWED_ACE_TYPE
            || ace_type == ACCESS_ALLOWED_CALLBACK_ACE_TYPE
        {
            (ace.sid.as_ref(), ace.mask)
        } else if [
            ACCESS_DENIED_ACE_TYPE,
            ACCESS_DENIED_CALLBACK_ACE_TYPE,
            ACCESS_DENIED_OBJECT_ACE_TYPE,
            ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE,
        ]
        .contains(&ace_type)
        {
            // a denial lets no one in
            continue;
        } else {
            // an allow type that is not read here: every account, every right
            (None, u32::MAX)
        };
        let who = trustee(sid, sec.owner.as_ref(), me);
        if who == Trustee::Nobody || mask == 0 {
            continue;
        }
        p.owner_only = false;
        p.shared_writable |= mask & WRITE_RIGHTS != 0;
        p.open_to_all |= who == Trustee::All && mask & open_rights != 0;
    }
    p
}

/// This process's account: the user SID of its token, read once.
pub fn current_account() -> io::Result<Sid> {
    static ACCOUNT: OnceLock<Sid> = OnceLock::new();
    if let Some(sid) = ACCOUNT.get() {
        return Ok(*sid);
    }
    let sid = token_user()?;
    Ok(*ACCOUNT.get_or_init(|| sid))
}

/// The user SID of this process's token, read into a fixed buffer that holds a `TOKEN_USER`
/// and the largest SID. The SID's place in the buffer is checked before it is read.
#[allow(unsafe_code)]
fn token_user() -> io::Result<Sid> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the process pseudo handle needs no closing; `token` is a writable handle slot.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so `token` is a new handle that nothing else owns.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut buf = [0u64; (size_of::<TOKEN_USER>() + SID_MAX_BYTES).div_ceil(8)];
    let size = std::mem::size_of_val(&buf);
    let mut filled = 0u32;
    // SAFETY: the token is open for the call; the buffer is writable, 8-byte aligned (as
    // TOKEN_USER needs) and `size` bytes long.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buf.as_mut_ptr().cast(),
            size as u32,
            &mut filled,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded and filled a TOKEN_USER at the start of the aligned buffer.
    let user: TOKEN_USER = unsafe { buf.as_ptr().cast::<TOKEN_USER>().read() };
    let bytes: Vec<u8> = buf.iter().flat_map(|w| w.to_ne_bytes()).collect();
    let filled = (filled as usize).min(size);
    let at = (user.User.Sid as usize)
        .checked_sub(buf.as_ptr() as usize)
        .filter(|at| *at < filled)
        .ok_or_else(|| os_error(ERROR_INVALID_SID))?;
    Sid::parse(&bytes[at..filled])
}

/// A new handle to the file (never a directory: `ReOpenFile` refuses those) that `h` is open on,
/// with `access` and no data access, sharing everything, a reparse point not followed. It
/// reaches the same object, never a name again.
#[allow(unsafe_code)]
fn reopen_file(h: RawHandle, access: u32) -> io::Result<OwnedHandle> {
    // SAFETY: the caller holds `h` open for the call; no pointer is passed.
    let new = unsafe {
        ReOpenFile(
            h,
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        )
    };
    if new == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so `new` is a new handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(new) })
}

/// The kind of the object `h` is open on, and whether it is a reparse point
/// (`FILE_ATTRIBUTE_TAG_INFO`). It reads a handle open for reading, writing or appending.
#[allow(unsafe_code)]
fn kind_of(h: RawHandle) -> io::Result<(ObjectKind, bool)> {
    let mut info = FILE_ATTRIBUTE_TAG_INFO {
        FileAttributes: 0,
        ReparseTag: 0,
    };
    // SAFETY: `h` is open for the call; the buffer is writable and of the size passed, and is
    // the struct that FileAttributeTagInfo fills.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            h,
            FileAttributeTagInfo,
            (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let kind = if info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        ObjectKind::Dir
    } else {
        ObjectKind::File
    };
    Ok((
        kind,
        info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
    ))
}

/// The owner and the DACL of the object `h` is open on (`h` has `READ_CONTROL`), read whole
/// into a buffer of at most [`DESCRIPTOR_MAX_BYTES`].
#[allow(unsafe_code)]
fn read_security(h: RawHandle) -> io::Result<Security> {
    let what = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let mut small = [0u32; 256];
    let mut needed = 0u32;
    // SAFETY: `h` is open for the call; the buffer is writable, 4-byte aligned and of the size
    // passed; `needed` is a writable u32.
    let ok =
        unsafe { GetKernelObjectSecurity(h, what, small.as_mut_ptr().cast(), 1024, &mut needed) };
    if ok != 0 {
        let bytes: Vec<u8> = small.iter().flat_map(|w| w.to_le_bytes()).collect();
        return parse_security(&bytes);
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
        return Err(e);
    }
    let len = needed as usize;
    if len == 0 || len > DESCRIPTOR_MAX_BYTES {
        return Err(os_error(ERROR_INVALID_SECURITY_DESCR));
    }
    let mut big = vec![0u32; len.div_ceil(4)];
    // SAFETY: as above, with a buffer of at least `len` bytes.
    let ok =
        unsafe { GetKernelObjectSecurity(h, what, big.as_mut_ptr().cast(), needed, &mut needed) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let bytes: Vec<u8> = big.iter().flat_map(|w| w.to_le_bytes()).collect();
    parse_security(&bytes[..len])
}

/// Sets the DACL of the object `h` is open on (`h` has `WRITE_DAC`) to the one in `sd`, a
/// self-relative descriptor, as it is: protected when `sd` says so. Inheritable ACEs are not
/// pushed to what the directory holds, as a mode change does not touch it on Unix.
#[allow(unsafe_code)]
fn set_dacl(h: RawHandle, sd: *mut c_void) -> io::Result<()> {
    // SAFETY: `h` is open for the call and `sd` is a valid descriptor that outlives it.
    if unsafe { SetKernelObjectSecurity(h, DACL_SECURITY_INFORMATION, sd) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Who may use the open file or directory `f`, read from its handle, which has `READ_CONTROL`
/// (as every open for reading, writing or appending has): its owner, DACL, kind and reparse
/// attribute, judged as [`Privacy`] describes.
///
/// - `private`: the DACL is protected and holds exactly the one ACE of a private object of its
///   kind ([`Ace::private`]), and it is no reparse point.
/// - `owner_only`: no ACE that applies to it lets another account in. `SYSTEM` and
///   `BUILTIN\Administrators` count as no other account: they are the Unix superuser.
/// - `shared_writable`: another account holds a right that changes it ([`WRITE_RIGHTS`]).
/// - `open_to_all`: a group of every account may write a file, or remove or replace an entry of
///   a directory ([`REPLACE_ENTRY_RIGHTS`]); one that may only add entries does not.
///
/// A missing or null DACL lets every account in. A failed read is an error.
pub fn privacy(f: &File) -> io::Result<Privacy> {
    let (kind, reparse) = kind_of(f.as_raw_handle())?;
    let sec = read_security(f.as_raw_handle())?;
    Ok(judge(&sec, kind, reparse, &current_account()?))
}

/// The owner and the DACL of the open file or directory `f`, read from its handle, which has
/// `READ_CONTROL`: for tests and diagnostics.
pub fn security_of(f: &File) -> io::Result<Security> {
    read_security(f.as_raw_handle())
}

/// How [`open_no_follow`] opens a file: as `opts` says, or ([`private_file`]) a missing file is
/// made private first.
pub struct NoFollow<'a> {
    opts: &'a mut OpenOptions,
    private: bool,
}

impl<'a> From<&'a mut OpenOptions> for NoFollow<'a> {
    fn from(opts: &'a mut OpenOptions) -> NoFollow<'a> {
        NoFollow {
            opts,
            private: false,
        }
    }
}

/// `opts`, for options that create a file (`create` or `create_new`): a file that
/// [`open_no_follow`] makes is private from the moment it exists. It is made new with this
/// account's [`PrivateDescriptor`], then opened as `opts` says, and the open must reach the file
/// that was made. A file that already exists is opened as `opts` says and keeps its security:
/// [`make_private_file`] narrows it. (On Unix `opts` gets mode 0o600; std cannot pass a security
/// descriptor through `OpenOptions` on Windows.)
pub fn private_file(opts: &mut OpenOptions) -> NoFollow<'_> {
    NoFollow {
        opts,
        private: true,
    }
}

/// Opens `path` as `opts` says, never through a reparse point: the last entry is opened itself
/// (`FILE_FLAG_OPEN_REPARSE_POINT`; a directory opens too), and a reparse point of any tag is
/// refused, `{path} is a reparse point; rdy will not follow it`. So a checkout cannot point a run
/// file or a config file at a file elsewhere for rdy to write, secure or read.
pub fn open_no_follow<'a>(opts: impl Into<NoFollow<'a>>, path: &Path) -> io::Result<File> {
    let NoFollow { opts, private } = opts.into();
    if private {
        open_private(opts, path)
    } else {
        open_plain(opts, path)
    }
}

/// [`open_no_follow`] without the private create.
fn open_plain(opts: &mut OpenOptions, path: &Path) -> io::Result<File> {
    let f = opts
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    if kind_of(f.as_raw_handle())?.1 {
        return Err(reparse_point(path));
    }
    Ok(f)
}

/// [`open_no_follow`] for [`private_file`]: makes `path` new and private, with no data access,
/// sharing no delete, so it cannot be removed or renamed while it is opened as `opts` says;
/// then checks that the open reached it. A file that exists already is opened as `opts` says.
/// If the second open fails, its error is returned and the new file stays, empty and private, as
/// a file that `O_CREAT` made stays on Unix when a later step fails.
#[allow(unsafe_code)]
fn open_private(opts: &mut OpenOptions, path: &Path) -> io::Result<File> {
    let sd = PrivateDescriptor::of_this_account(ObjectKind::File).map_err(|e| not_made(path, e))?;
    let attributes = sd.attributes();
    let name = native(path)?;
    // SAFETY: `name` is NUL-terminated and `attributes` and the descriptor it points to live
    // for the call; no template handle is passed.
    let made = unsafe {
        CreateFileW(
            name.as_ptr(),
            READ_CONTROL | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &attributes,
            CREATE_NEW,
            // a directory or a link to one at `path` is then an existing entry, not a refusal
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if made == INVALID_HANDLE_VALUE {
        let e = io::Error::last_os_error();
        return if e.kind() == io::ErrorKind::AlreadyExists {
            open_plain(opts, path)
        } else {
            Err(e)
        };
    }
    // SAFETY: the call succeeded, so `made` is a new handle that nothing else owns.
    let made = File::from(unsafe { OwnedHandle::from_raw_handle(made) });
    // the file exists now, new and empty: open it as it stands
    let f = open_plain(opts.create_new(false).create(false).truncate(false), path)?;
    if FileId::of_open(&f, path)? != FileId::of_open(&made, path)? {
        return Err(parent_changed(path));
    }
    Ok(f)
}

/// Makes the open file `f` private to this account (the descriptor of a private object of its
/// kind), when this account owns it: a file left by an older run keeps its security through
/// open. One that is private already is left as it is. One that another account owns is refused,
/// `{path} is not private to this account`, and never changed; a reparse point is refused.
/// `{path}` is the object's final path. Any other failure is `could not make {path} private to
/// this account: {error}`.
pub fn make_private_file(f: &File) -> io::Result<()> {
    let path = final_path(f.as_raw_handle())?;
    make_private(f, &path)
}

/// Creates the directory `path`, private to this account from the moment it exists (its
/// [`PrivateDescriptor`]); an existing `path` is an error, as with `DirBuilder::create`.
#[allow(unsafe_code)]
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    let sd = PrivateDescriptor::of_this_account(ObjectKind::Dir).map_err(|e| not_made(path, e))?;
    let attributes = sd.attributes();
    let name = native(path)?;
    // SAFETY: `name` is NUL-terminated and `attributes` and the descriptor it points to live
    // for the call.
    if unsafe { CreateDirectoryW(name.as_ptr(), &attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Makes the directory `path` private to this account, as [`make_private_file`] does a file:
/// opened without following a reparse point, which is refused, and only when this account owns
/// it.
pub fn make_private_dir(path: &Path) -> io::Result<()> {
    let dir = OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    make_private(&dir, path)
}

/// Makes the object `f` is open on (with `READ_CONTROL`) private to this account; `path` names
/// it in errors.
fn make_private(f: &File, path: &Path) -> io::Result<()> {
    let (kind, reparse) = kind_of(f.as_raw_handle()).map_err(|e| not_made(path, e))?;
    if reparse {
        return Err(reparse_point(path));
    }
    let me = current_account().map_err(|e| not_made(path, e))?;
    let sec = read_security(f.as_raw_handle()).map_err(|e| not_made(path, e))?;
    if sec.owner != Some(me) {
        return Err(not_private(path));
    }
    if judge(&sec, kind, false, &me).private {
        return Ok(());
    }
    let access = WRITE_DAC | READ_CONTROL | FILE_READ_ATTRIBUTES;
    let w = match kind {
        ObjectKind::File => reopen_file(f.as_raw_handle(), access)
            .map(File::from)
            .map_err(|e| not_made(path, e))?,
        ObjectKind::Dir => reopen_dir(f, path, access)?,
    };
    let sd = PrivateDescriptor::new(&me, kind);
    set_dacl(w.as_raw_handle(), sd.as_ptr()).map_err(|e| not_made(path, e))?;
    let after = read_security(w.as_raw_handle()).map_err(|e| not_made(path, e))?;
    if after.owner != Some(me) || !judge(&after, kind, false, &me).private {
        return Err(not_private(path));
    }
    Ok(())
}

/// A new handle with `access` to the directory `dir`, opened again by `path` (`ReOpenFile`
/// refuses a directory). It must reach the same directory, never a reparse point; another one
/// at `path` now is `the directory holding {path} changed during the operation`.
fn reopen_dir(dir: &File, path: &Path, access: u32) -> io::Result<File> {
    let again = OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|e| not_made(path, e))?;
    if kind_of(again.as_raw_handle())
        .map_err(|e| not_made(path, e))?
        .1
    {
        return Err(reparse_point(path));
    }
    if FileId::of_open(&again, path)? != FileId::of_open(dir, path)? {
        return Err(parent_changed(path));
    }
    Ok(again)
}

/// The final path of the object `h` is open on, as a drive or UNC path for messages. A path
/// longer than [`NATIVE_PATH_MAX`] is `ERROR_FILENAME_EXCED_RANGE`, before any buffer for it is
/// made.
#[allow(unsafe_code)]
fn final_path(h: RawHandle) -> io::Result<PathBuf> {
    let call = |buf: &mut [u16]| {
        // SAFETY: `h` is open for the call; the buffer is writable for the length passed.
        unsafe {
            GetFinalPathNameByHandleW(
                h,
                buf.as_mut_ptr(),
                buf.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        }
    };
    let mut small = [0u16; 512];
    let n = call(&mut small) as usize;
    let wide = match n {
        0 => return Err(io::Error::last_os_error()),
        n if n < small.len() => small[..n].to_vec(),
        // too small: `n` is the length needed, with the NUL
        n if n > NATIVE_PATH_MAX + 1 => return Err(os_error(ERROR_FILENAME_EXCED_RANGE)),
        n => {
            let mut big = vec![0u16; n];
            match call(&mut big) as usize {
                0 => return Err(io::Error::last_os_error()),
                m if m < n => {
                    big.truncate(m);
                    big
                }
                _ => return Err(os_error(ERROR_INSUFFICIENT_BUFFER)),
            }
        }
    };
    let unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
    let local: Vec<u16> = r"\\?\".encode_utf16().collect();
    let shown = if let Some(rest) = wide.strip_prefix(&unc[..]) {
        [&[u16::from(b'\\'); 2][..], rest].concat()
    } else if let Some(rest) = wide.strip_prefix(&local[..]) {
        rest.to_vec()
    } else {
        wide
    };
    Ok(PathBuf::from(OsString::from_wide(&shown)))
}

/// `path` as a NUL-terminated native path for a call that takes one: made absolute, then
/// written in the `\\?\` form, so a long path is not cut at 260 units. A NUL in it, or a native
/// path longer than [`NATIVE_PATH_MAX`], is refused before the native path is made.
fn native(path: &Path) -> io::Result<Vec<u16>> {
    if path.as_os_str().encode_wide().any(|u| u == 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("the native path for {} contains a NUL", path.display()),
        ));
    }
    let too_long = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "the native path for {} exceeds 32767 UTF-16 code units",
                path.display()
            ),
        )
    };
    if path.as_os_str().encode_wide().count() > NATIVE_PATH_MAX {
        return Err(too_long());
    }
    let absolute = std::path::absolute(path)?;
    let units = || absolute.as_os_str().encode_wide();
    let starts = |prefix: &str| units().take(prefix.len()).eq(prefix.encode_utf16());
    let (prefix, skip) = if starts(r"\\?\") || starts(r"\\.\") {
        ("", 0)
    } else if starts(r"\\") {
        (r"\\?\UNC\", 2)
    } else {
        (r"\\?\", 0)
    };
    let len = prefix.len() + units().count() - skip;
    if len > NATIVE_PATH_MAX {
        return Err(too_long());
    }
    let mut out = Vec::with_capacity(len + 1);
    out.extend(prefix.encode_utf16());
    out.extend(units().skip(skip));
    out.push(0);
    Ok(out)
}

/// The OS error `code`.
fn os_error(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

/// `could not make {path} private to this account: {error}`, of the same kind as `e`.
fn not_made(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(
        e.kind(),
        format!(
            "could not make {} private to this account: {e}",
            path.display()
        ),
    )
}

/// `{path} is not private to this account`.
fn not_private(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{} is not private to this account", path.display()),
    )
}

/// `{path} is a reparse point; rdy will not follow it`.
fn reparse_point(path: &Path) -> io::Error {
    io::Error::other(format!(
        "{} is a reparse point; rdy will not follow it",
        path.display()
    ))
}

/// `the directory holding {path} changed during the operation`.
fn parent_changed(path: &Path) -> io::Error {
    io::Error::other(format!(
        "the directory holding {} changed during the operation",
        path.display()
    ))
}

/// Native NTFS cases of the provider itself: the descriptor it makes, hostile descriptors, the
/// judgement of every kind of ACE, every reparse tag, inherited access, a failed ACL setup and
/// objects of other accounts.
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::windows::fs::{symlink_dir, symlink_file};
    use windows_sys::Win32::Security::{INHERITED_ACE, IsValidSecurityDescriptor};
    use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;
    use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_OBJECT_ACE_TYPE;

    const PRIVATE: Privacy = Privacy {
        mine: true,
        private: true,
        owner_only: true,
        shared_writable: false,
        open_to_all: false,
    };

    fn me() -> Sid {
        current_account().unwrap()
    }

    fn open_read(path: &Path) -> File {
        open_no_follow(OpenOptions::new().read(true), path).unwrap()
    }

    /// `path` itself, a reparse point not followed, opened to read its security.
    fn open_itself(path: &Path) -> File {
        OpenOptions::new()
            .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap()
    }

    fn allow(sid: Sid, flags: u32, mask: u32) -> Ace {
        Ace {
            kind: ACCESS_ALLOWED_ACE_TYPE as u8,
            flags: flags as u8,
            mask,
            sid: Some(sid),
        }
    }

    /// Sets the DACL of what `path` names to `aces`, protected, through a handle with WRITE_DAC.
    fn set_aces(path: &Path, aces: &[Ace]) {
        let h = OpenOptions::new()
            .access_mode(WRITE_DAC | READ_CONTROL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap();
        let words: Vec<u32> = descriptor_bytes(&me(), aces)
            .chunks(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        set_dacl(h.as_raw_handle(), words.as_ptr() as *mut c_void).unwrap();
    }

    /// Runs `program` with `args` and checks that it succeeded.
    fn run(program: &str, args: &[&std::ffi::OsStr]) {
        let out = std::process::Command::new(program)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{program} {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Gives Everyone and Users full access to `dir`, inherited by what is made in it.
    fn open_to_all(dir: &Path) {
        for sid in ["*S-1-1-0", "*S-1-5-32-545"] {
            run(
                "icacls",
                &[
                    dir.as_os_str(),
                    "/grant".as_ref(),
                    format!("{sid}:(OI)(CI)(F)").as_ref(),
                ],
            );
        }
    }

    /// Makes `path` (an empty file or directory) a reparse point of the non-Microsoft tag `tag`,
    /// through the GUID form that such a tag needs.
    #[allow(unsafe_code)]
    fn set_custom_tag(path: &Path, tag: u32) {
        let f = OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap();
        let mut buf = Vec::new();
        buf.extend(tag.to_le_bytes());
        buf.extend(4u16.to_le_bytes());
        buf.extend(0u16.to_le_bytes());
        buf.extend(*b"rdy-wx4-reparse!");
        buf.extend(*b"data");
        let mut returned = 0u32;
        // SAFETY: the handle is open for the call; the input buffer is readable for its length;
        // no output buffer or overlapped struct is passed.
        let ok = unsafe {
            DeviceIoControl(
                f.as_raw_handle(),
                FSCTL_SET_REPARSE_POINT,
                buf.as_ptr().cast(),
                buf.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0, "{}: {}", path.display(), io::Error::last_os_error());
    }

    #[test]
    fn wx4_native_the_private_descriptor_is_valid_and_exact() {
        let me = me();
        for (kind, flags) in [(ObjectKind::File, 0u8), (ObjectKind::Dir, 0x03)] {
            let sd = PrivateDescriptor::new(&me, kind);
            // SAFETY: the descriptor lives for the call.
            #[allow(unsafe_code)]
            let valid = unsafe { IsValidSecurityDescriptor(sd.as_ptr()) };
            assert_ne!(valid, 0, "{kind:?}: Windows accepts it");
            let bytes = sd.to_bytes();
            let sid = me.as_bytes().len();
            assert_eq!(
                bytes.len(),
                20 + sid + 8 + 8 + sid,
                "{kind:?}: header, owner, ACL, ACE"
            );
            let sec = parse_security(&bytes).unwrap();
            assert_eq!(sec.owner, Some(me));
            assert!(sec.protected);
            assert_eq!(
                sec.dacl,
                Some(vec![Ace {
                    kind: 0,
                    flags,
                    mask: 0x001F_01FF,
                    sid: Some(me)
                }]),
                "{kind:?}: one allow ACE, this account, every right"
            );
            assert_eq!(judge(&sec, kind, false, &me), PRIVATE);
        }
    }

    #[test]
    fn wx4_native_hostile_descriptors_are_bounded_errors() {
        let good = PrivateDescriptor::new(&me(), ObjectKind::Dir).to_bytes();
        let codes = [
            ERROR_INVALID_SECURITY_DESCR,
            ERROR_INVALID_ACL,
            ERROR_INVALID_SID,
        ]
        .map(|c| Some(c as i32));
        let refused = |sd: &[u8], what: &str| {
            let e = parse_security(sd).unwrap_err();
            assert!(codes.contains(&e.raw_os_error()), "{what}: {e}");
            assert_ne!(e.kind(), io::ErrorKind::InvalidData, "{what}");
        };
        // every cut is refused: the ACL's size reaches the end
        for n in 0..good.len() {
            refused(&good[..n], &format!("cut at {n}"));
        }
        let sid = me().as_bytes().len();
        let (acl, ace) = (20 + sid, 20 + sid + 8);
        let with = |at: usize, bytes: &[u8]| {
            let mut sd = good.clone();
            sd[at..at + bytes.len()].copy_from_slice(bytes);
            sd
        };
        let cases = [
            ("revision 2", with(0, &[2])),
            ("not self-relative", with(3, &[0x10])),
            (
                "owner past the end",
                with(4, &(good.len() as u32).to_le_bytes()),
            ),
            ("owner offset at 4 GiB", with(4, &u32::MAX.to_le_bytes())),
            ("16 owner sub-authorities", with(21, &[16])),
            ("owner revision 2", with(20, &[2])),
            (
                "DACL past the end",
                with(16, &(good.len() as u32 + 4).to_le_bytes()),
            ),
            ("ACL revision 9", with(acl, &[9])),
            ("ACL size 65535", with(acl + 2, &u16::MAX.to_le_bytes())),
            ("ACL size 4", with(acl + 2, &4u16.to_le_bytes())),
            ("65535 ACEs", with(acl + 4, &u16::MAX.to_le_bytes())),
            ("2 ACEs, room for 1", with(acl + 4, &2u16.to_le_bytes())),
            ("ACE size 4", with(ace + 2, &4u16.to_le_bytes())),
            (
                "ACE past the ACL",
                with(ace + 2, &(8 + sid as u16 + 4).to_le_bytes()),
            ),
            ("ACE SID of 15 sub-authorities", with(ace + 9, &[15])),
        ];
        for (what, sd) in cases {
            refused(&sd, what);
        }
        // no owner, or no DACL, is read as such, not as damage
        let no_owner = parse_security(&with(4, &[0; 4])).unwrap();
        assert_eq!(no_owner.owner, None);
        let no_dacl = parse_security(&with(2, &[0x00, 0x90])).unwrap();
        assert_eq!(no_dacl.dacl, None, "SE_DACL_PRESENT clear");
        let p = judge(&no_dacl, ObjectKind::Dir, false, &me());
        assert!(!p.private && !p.owner_only && p.shared_writable && p.open_to_all);
    }

    #[test]
    fn wx4_native_sids_are_bounded_and_written_in_the_standard_form() {
        assert_eq!(Sid::of(5, &[18]).to_string(), "S-1-5-18");
        assert_eq!(Sid::of(1, &[0]).to_string(), "S-1-1-0");
        let user = Sid::of(5, &[21, 1, 2, u32::MAX, 1001]);
        assert_eq!(user.to_string(), "S-1-5-21-1-2-4294967295-1001");
        let mut big = vec![1, 1, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc];
        big.extend(7u32.to_le_bytes());
        assert_eq!(
            Sid::parse(&big).unwrap().to_string(),
            "S-1-0x123456789ABC-7"
        );
        // the largest SID fits; no SID is longer
        let mut max = vec![1, 15, 0, 0, 0, 0, 0, 5];
        max.extend([7; 60]);
        assert_eq!(Sid::parse(&max).unwrap().as_bytes().len(), SID_MAX_BYTES);
        max.extend([0; 40]);
        assert_eq!(
            Sid::parse(&max).unwrap().as_bytes().len(),
            68,
            "the rest is not read"
        );
        for (what, bad) in [
            ("empty", vec![]),
            ("one byte", vec![1]),
            (
                "16 sub-authorities",
                [vec![1, 16, 0, 0, 0, 0, 0, 5], vec![0; 64]].concat(),
            ),
            ("revision 2", vec![2, 0, 0, 0, 0, 0, 0, 5]),
            ("cut", vec![1, 2, 0, 0, 0, 0, 0, 5, 1, 0, 0, 0]),
        ] {
            let e = Sid::parse(&bad).unwrap_err();
            assert_eq!(e.raw_os_error(), Some(ERROR_INVALID_SID as i32), "{what}");
        }
        // this account is the token's user, as whoami reads it
        let out = std::process::Command::new("whoami")
            .args(["/user", "/fo", "csv", "/nh"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let sid = text.trim().rsplit(',').next().unwrap().trim_matches('"');
        assert_eq!(me().to_string(), sid);
    }

    #[test]
    fn wx4_native_privacy_judges_every_ace() {
        let me = me();
        let other = Sid::of(5, &[21, 1, 2, 3, 1002]);
        let everyone = Sid::of(1, &[0]);
        let users = Sid::of(5, &[32, 545]);
        let system = Sid::of(5, &[18]);
        let admins = Sid::of(5, &[32, 544]);
        let owner_rights = Sid::of(3, &[4]);
        let mine = |dacl: Vec<Ace>| Security {
            owner: Some(me),
            protected: true,
            dacl: Some(dacl),
        };
        let all = FILE_ALL_ACCESS;
        let file = ObjectKind::File;
        let dir = ObjectKind::Dir;
        // (what, descriptor, kind, reparse, private, owner_only, shared_writable, open_to_all)
        let cases = [
            (
                "private file",
                mine(vec![allow(me, 0, all)]),
                file,
                false,
                true,
                true,
                false,
                false,
            ),
            (
                "private dir",
                mine(vec![allow(me, 3, all)]),
                dir,
                false,
                true,
                true,
                false,
                false,
            ),
            (
                "a file's ACE on a dir",
                mine(vec![allow(me, 0, all)]),
                dir,
                false,
                false,
                true,
                false,
                false,
            ),
            (
                "a reparse point",
                mine(vec![allow(me, 0, all)]),
                file,
                true,
                false,
                true,
                false,
                false,
            ),
            (
                "inherited",
                Security {
                    protected: false,
                    ..mine(vec![allow(me, INHERITED_ACE, all)])
                },
                file,
                false,
                false,
                true,
                false,
                false,
            ),
            (
                "unprotected",
                Security {
                    protected: false,
                    ..mine(vec![allow(me, 0, all)])
                },
                file,
                false,
                false,
                true,
                false,
                false,
            ),
            (
                "not every right",
                mine(vec![allow(me, 0, FILE_GENERIC_READ)]),
                file,
                false,
                false,
                true,
                false,
                false,
            ),
            (
                "SYSTEM and Administrators too",
                mine(vec![
                    allow(me, 0, all),
                    allow(system, 0, all),
                    allow(admins, 0, all),
                ]),
                file,
                false,
                false,
                true,
                false,
                false,
            ),
            (
                "OWNER RIGHTS of this account",
                mine(vec![allow(me, 0, all), allow(owner_rights, 0, all)]),
                file,
                false,
                false,
                true,
                false,
                false,
            ),
            (
                "Everyone may read",
                mine(vec![
                    allow(me, 0, all),
                    allow(everyone, 0, FILE_GENERIC_READ),
                ]),
                file,
                false,
                false,
                false,
                false,
                false,
            ),
            (
                "another account may write",
                mine(vec![allow(me, 0, all), allow(other, 0, FILE_GENERIC_WRITE)]),
                file,
                false,
                false,
                false,
                true,
                false,
            ),
            (
                "Users may write a file",
                mine(vec![allow(me, 0, all), allow(users, 0, FILE_GENERIC_WRITE)]),
                file,
                false,
                false,
                false,
                true,
                true,
            ),
            (
                "Users may add entries only (sticky)",
                mine(vec![
                    allow(me, 3, all),
                    allow(users, 0, FILE_GENERIC_READ | FILE_WRITE_DATA),
                ]),
                dir,
                false,
                false,
                false,
                true,
                false,
            ),
            (
                "Everyone may remove entries",
                mine(vec![
                    allow(me, 3, all),
                    allow(everyone, 0, FILE_DELETE_CHILD),
                ]),
                dir,
                false,
                false,
                false,
                true,
                true,
            ),
            (
                "Everyone, inherit-only",
                mine(vec![
                    allow(me, 3, all),
                    allow(everyone, INHERIT_ONLY_ACE | 3, all),
                ]),
                dir,
                false,
                false,
                true,
                false,
                false,
            ),
            (
                "Everyone denied",
                mine(vec![
                    Ace {
                        kind: ACCESS_DENIED_ACE_TYPE as u8,
                        ..allow(everyone, 0, all)
                    },
                    allow(me, 0, all),
                ]),
                file,
                false,
                false,
                true,
                false,
                false,
            ),
            (
                "an allow object ACE, not read",
                mine(vec![
                    allow(me, 0, all),
                    Ace {
                        kind: ACCESS_ALLOWED_OBJECT_ACE_TYPE as u8,
                        flags: 0,
                        mask: 1,
                        sid: None,
                    },
                ]),
                dir,
                false,
                false,
                false,
                true,
                true,
            ),
            (
                "a null DACL",
                Security {
                    dacl: None,
                    ..mine(vec![])
                },
                dir,
                false,
                false,
                false,
                true,
                true,
            ),
            (
                "an empty DACL",
                mine(vec![]),
                file,
                false,
                false,
                true,
                false,
                false,
            ),
        ];
        for (what, sec, kind, reparse, private, owner_only, shared, open) in cases {
            let p = judge(&sec, kind, reparse, &me);
            assert_eq!(
                (
                    p.mine,
                    p.private,
                    p.owner_only,
                    p.shared_writable,
                    p.open_to_all
                ),
                (true, private, owner_only, shared, open),
                "{what}"
            );
        }
        // another account's object is not mine, whatever its DACL; OWNER RIGHTS is then theirs
        let theirs = Security {
            owner: Some(other),
            protected: true,
            dacl: Some(vec![allow(me, 0, all), allow(owner_rights, 0, all)]),
        };
        let p = judge(&theirs, file, false, &me);
        assert_eq!(
            (p.mine, p.private, p.owner_only, p.shared_writable),
            (false, false, false, true)
        );
        let no_owner = Security {
            owner: None,
            ..mine(vec![allow(me, 0, all)])
        };
        assert!(!judge(&no_owner, file, false, &me).mine);
    }

    #[test]
    fn wx4_native_reparse_points_of_every_tag_are_refused() {
        let d = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let victim = elsewhere.path().join("victim");
        fs::write(&victim, b"keep").unwrap();
        let victim_dir = elsewhere.path().join("victim-dir");
        fs::create_dir(&victim_dir).unwrap();
        let before = (
            security_of(&open_read(&victim)).unwrap(),
            security_of(&open_read(&victim_dir)).unwrap(),
        );
        let at = |n: &str| d.path().join(n);
        symlink_file(&victim, at("symlink-file")).unwrap();
        symlink_dir(&victim_dir, at("symlink-dir")).unwrap();
        symlink_file(elsewhere.path().join("missing"), at("dangling")).unwrap();
        run(
            "cmd",
            &[
                "/C".as_ref(),
                "mklink".as_ref(),
                "/J".as_ref(),
                at("junction").as_os_str(),
                victim_dir.as_os_str(),
            ],
        );
        fs::write(at("custom-file"), b"").unwrap();
        set_custom_tag(&at("custom-file"), 0x0000_7a57);
        fs::create_dir(at("custom-dir")).unwrap();
        set_custom_tag(&at("custom-dir"), 0x0000_7a58);
        let dirs = ["symlink-dir", "junction", "custom-dir"];
        for name in [
            "symlink-file",
            "dangling",
            "symlink-dir",
            "junction",
            "custom-file",
            "custom-dir",
        ] {
            let path = at(name);
            let refusal = format!(
                "{} is a reparse point; rdy will not follow it",
                path.display()
            );
            let mut opens: Vec<(&str, OpenOptions)> = Vec::new();
            for (how, read, write, append, create, truncate) in [
                ("read", true, false, false, false, false),
                ("write", false, true, false, false, false),
                ("append, create", false, false, true, true, false),
                ("write, create, truncate", false, true, false, true, true),
                ("write, truncate", false, true, false, false, true),
            ] {
                let mut o = OpenOptions::new();
                o.read(read)
                    .write(write)
                    .append(append)
                    .create(create)
                    .truncate(truncate);
                opens.push((how, o));
            }
            for (how, mut o) in opens {
                let e = open_no_follow(&mut o, &path).unwrap_err();
                if dirs.contains(&name) && how.contains("truncate") {
                    // Windows itself refuses to open a directory to truncate it
                    assert_eq!(e.raw_os_error(), Some(87), "{name}, {how}: {e}");
                } else {
                    assert_eq!(e.to_string(), refusal, "{name}, {how}");
                }
            }
            let e = open_no_follow(
                private_file(OpenOptions::new().write(true).create_new(true)),
                &path,
            )
            .unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{name}: {e}");
            let e = open_no_follow(
                private_file(
                    OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(false),
                ),
                &path,
            )
            .unwrap_err();
            assert_eq!(e.to_string(), refusal, "{name}: private create");
            // the entry itself: never private, never made private
            let itself = open_itself(&path);
            assert!(!privacy(&itself).unwrap().private, "{name}");
            let e = make_private_file(&itself).unwrap_err();
            assert!(
                e.to_string()
                    .ends_with(" is a reparse point; rdy will not follow it"),
                "{name}: {e}"
            );
            if dirs.contains(&name) {
                let e = make_private_dir(&path).unwrap_err();
                assert_eq!(e.to_string(), refusal, "{name}: make_private_dir");
            }
            // still a reparse point: nothing replaced it
            assert!(
                kind_of(itself.as_raw_handle()).unwrap().1,
                "{name} is still a reparse point"
            );
        }
        assert_eq!(fs::read(&victim).unwrap(), b"keep");
        assert_eq!(fs::read_dir(&victim_dir).unwrap().count(), 0);
        assert!(
            !elsewhere.path().join("missing").exists(),
            "a dangling link made nothing"
        );
        let after = (
            security_of(&open_read(&victim)).unwrap(),
            security_of(&open_read(&victim_dir)).unwrap(),
        );
        assert_eq!(after, before, "the targets keep their security");
    }

    #[test]
    fn wx4_native_inherited_access_is_dropped_and_never_taken() {
        let d = tempfile::tempdir().unwrap();
        let open = d.path().join("open");
        fs::create_dir(&open).unwrap();
        open_to_all(&open);
        let s = security_of(&open_read(&open)).unwrap();
        assert!(
            s.dacl
                .unwrap()
                .iter()
                .any(|a| a.sid == Some(Sid::of(1, &[0]))),
            "Everyone"
        );
        // a plain file inherits Everyone and Users
        let plain = open.join("plain");
        fs::write(&plain, b"plain").unwrap();
        let s = security_of(&open_read(&plain)).unwrap();
        assert!(!s.protected);
        assert!(
            s.dacl
                .as_ref()
                .unwrap()
                .iter()
                .all(|a| u32::from(a.flags) & INHERITED_ACE != 0)
        );
        let p = privacy(&open_read(&plain)).unwrap();
        assert!(
            p.mine && !p.private && !p.owner_only && p.shared_writable && p.open_to_all,
            "{p:?}"
        );
        // a private file and a private directory inherit none of it
        let new = open.join("new");
        let f = open_no_follow(
            private_file(OpenOptions::new().write(true).create_new(true)),
            &new,
        )
        .unwrap();
        let only = |f: &File, kind| {
            let s = security_of(f).unwrap();
            assert_eq!(
                (s.owner, s.protected, s.dacl),
                (Some(me()), true, Some(vec![Ace::private(me(), kind)]))
            );
            assert_eq!(privacy(f).unwrap(), PRIVATE);
        };
        only(&f, ObjectKind::File);
        drop(f);
        let run_dir = open.join("run");
        create_private_dir(&run_dir).unwrap();
        only(&open_read(&run_dir), ObjectKind::Dir);
        // what is made in a private directory inherits only this account
        let child = run_dir.join("child");
        fs::write(&child, b"c").unwrap();
        let p = privacy(&open_read(&child)).unwrap();
        assert!(
            p.mine && !p.private && p.owner_only && !p.shared_writable,
            "{p:?}"
        );
        // existing objects are made private, the plain file and the open directory itself
        make_private_file(&open_read(&plain)).unwrap();
        only(&open_read(&plain), ObjectKind::File);
        make_private_dir(&open).unwrap();
        only(&open_read(&open), ObjectKind::Dir);
        // made private again: nothing changes
        make_private_dir(&open).unwrap();
        make_private_file(&open_read(&plain)).unwrap();
        only(&open_read(&plain), ObjectKind::File);
        assert_eq!(fs::read(&plain).unwrap(), b"plain");
        // a replacement keeps the descriptor of the private file renamed in
        let target = d.path().join("target");
        fs::write(&target, b"old").unwrap();
        run(
            "icacls",
            &[
                target.as_os_str(),
                "/grant".as_ref(),
                "*S-1-1-0:(F)".as_ref(),
            ],
        );
        assert!(privacy(&open_read(&target)).unwrap().open_to_all);
        fs::rename(&new, &target).unwrap();
        only(&open_read(&target), ObjectKind::File);
    }

    #[test]
    fn wx4_native_a_failed_acl_setup_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("f");
        fs::write(&path, b"x").unwrap();
        // OWNER RIGHTS limits the owner to reading: it may not change the DACL
        set_aces(&path, &[allow(Sid::of(3, &[4]), 0, FILE_GENERIC_READ)]);
        let f = open_read(&path);
        let before = security_of(&f).unwrap();
        let e = make_private_file(&f).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!(
                "could not make {} private to this account: {}",
                final_path(f.as_raw_handle()).unwrap().display(),
                io::Error::from_raw_os_error(5)
            )
        );
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(security_of(&f).unwrap(), before, "unchanged");
        assert!(!privacy(&f).unwrap().private);
        // the directory form: an existing directory the owner may not change
        let dir = d.path().join("dir");
        fs::create_dir(&dir).unwrap();
        set_aces(&dir, &[allow(Sid::of(3, &[4]), 0, FILE_GENERIC_READ)]);
        let e = make_private_dir(&dir).unwrap_err();
        assert!(
            e.to_string().starts_with(&format!(
                "could not make {} private to this account: ",
                dir.display()
            )),
            "{e}"
        );
        // a private create in a directory that refuses new entries makes nothing
        let e = open_no_follow(
            private_file(OpenOptions::new().write(true).create_new(true)),
            &dir.join("x"),
        )
        .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
        let e = create_private_dir(&dir.join("y")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
        // (the temp directory still removes both: its DACL lets this account delete entries)
    }

    #[test]
    fn wx4_native_another_account_is_never_repaired() {
        let root = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let hosts = root.join(r"System32\drivers\etc\hosts");
        for (path, is_dir) in [(&root, true), (&hosts, false)] {
            let f = open_read(path);
            let before = security_of(&f).unwrap();
            assert_ne!(before.owner, Some(me()), "{}", path.display());
            assert!(!privacy(&f).unwrap().mine);
            let e = if is_dir {
                make_private_dir(path).unwrap_err()
            } else {
                make_private_file(&f).unwrap_err()
            };
            assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
            assert!(
                e.to_string().ends_with(" is not private to this account"),
                "{e}"
            );
            assert_eq!(
                security_of(&f).unwrap(),
                before,
                "{} unchanged",
                path.display()
            );
        }
    }

    #[test]
    fn wx4_native_native_paths_are_bounded() {
        let e = native(Path::new("a\0b")).unwrap_err();
        assert_eq!(e.to_string(), "the native path for a\0b contains a NUL");
        let long = PathBuf::from("a".repeat(40_000));
        let e = native(&long).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!(
                "the native path for {} exceeds 32767 UTF-16 code units",
                long.display()
            )
        );
        let e = create_private_dir(&long).unwrap_err();
        assert!(
            e.to_string().ends_with("exceeds 32767 UTF-16 code units"),
            "{e}"
        );
        // a long path that fits is written in the \\?\ form and works
        let d = tempfile::tempdir().unwrap();
        let mut deep = d.path().to_path_buf();
        for _ in 0..12 {
            deep.push("a".repeat(30));
        }
        fs::create_dir_all(deep.parent().unwrap()).unwrap();
        create_private_dir(&deep).unwrap();
        assert_eq!(privacy(&open_read(&deep)).unwrap(), PRIVATE);
        let w = native(&deep).unwrap();
        assert!(String::from_utf16_lossy(&w).starts_with(r"\\?\"));
        assert_eq!(w.last(), Some(&0));
    }
}
