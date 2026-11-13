//! The Windows side of [`crate::pio`] (FW-XP1 Phase 4). A child module of this one makes each
//! part: `windows_io` makes the positioned reads and writes, hole punches and allocated-bytes
//! queries; `windows_security` makes private objects and no-follow opens; `windows_dir` makes the
//! opens, entry kinds, listings, creates, removals, renames and child processes anchored to an
//! open directory; `windows_durable` makes the directory flushes and write-through renames.
//! Permission-mode changes (`set_mode_at`) have no Windows form: Windows has no permission modes,
//! and a private Windows create already gives the object its private descriptor.

pub use windows_io::*;
// a sibling file: this file is not a mod.rs, so a plain `mod` would look in pio/windows/
#[path = "windows_io.rs"]
mod windows_io;

pub use windows_security::*;
#[path = "windows_security.rs"]
mod windows_security;

pub use windows_dir::*;
#[path = "windows_dir.rs"]
mod windows_dir;

pub use windows_durable::*;
#[path = "windows_durable.rs"]
mod windows_durable;

/// The longest host name read, in UTF-16 units with its end.
const MAX_HOST_NAME_UNITS: u32 = 1024;

/// This machine's DNS host name (`GetComputerNameExW` with `ComputerNameDnsHostname`), or `None`
/// when it cannot be read, is longer than 1023 UTF-16 units, is empty or is not Unicode.
#[allow(unsafe_code)]
pub fn host_name() -> Option<String> {
    use windows_sys::Win32::System::SystemInformation::{
        ComputerNameDnsHostname, GetComputerNameExW,
    };
    let mut buf = vec![0u16; MAX_HOST_NAME_UNITS as usize];
    let mut size = MAX_HOST_NAME_UNITS;
    // SAFETY: the buffer is valid for writes of `size` UTF-16 units; on success the call sets
    // `size` to the units written without the terminating NUL.
    let ok = unsafe { GetComputerNameExW(ComputerNameDnsHostname, buf.as_mut_ptr(), &mut size) };
    if ok == 0 {
        return None;
    }
    let name = String::from_utf16(buf.get(..size as usize)?).ok()?;
    (!name.is_empty()).then_some(name)
}
