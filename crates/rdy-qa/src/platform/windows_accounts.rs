//! A second account on Windows, for tests that check a private object against another account.
//!
//! - **The account is provisioned outside the product.** It is a standard local account that
//!   nobody logs on as. `RDY_QA_SECOND_ACCOUNT` names it, and `rdyother` is the default. A
//!   missing account is an error, never a skip, so `--require-all` fails.
//! - **The system judges, not a string.** [`granted_access`] asks the system's own access check
//!   (Authz) what the account would be granted on a real object. No test compares descriptor
//!   text. The account is never logged on: its groups are those the system computes for it.
//! - **A fixture proves its setup first.** [`share_with`] gives the account a right, and
//!   the same check must then grant it. Only after that does a test judge a private object.
//! - **This cannot run a process as the account.** It judges what the account is granted. A
//!   test that needs a running process of a standard second account still needs that logon.

use super::windows_fs::{own_sid, set_dacl, wide};
use rdy_store::pio::Sid;
use std::ffi::c_void;
use std::io;
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_SUCCESS, HANDLE, LUID, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    AUTHZ_ACCESS_REPLY, AUTHZ_ACCESS_REQUEST, AUTHZ_RM_FLAG_NO_AUDIT, AuthzAccessCheck,
    AuthzFreeContext, AuthzFreeResourceManager, AuthzInitializeContextFromSid,
    AuthzInitializeResourceManager, GetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, GetTokenInformation, LookupAccountNameW,
    OWNER_SECURITY_INFORMATION, SidTypeUser, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The environment variable that names the second account.
pub const SECOND_ACCOUNT_VAR: &str = "RDY_QA_SECOND_ACCOUNT";

/// Set to `1` by the hosted CI workflow only: that runner has no second account.
#[cfg(test)]
const HOSTED_CI_VAR: &str = "RDY_QA_HOSTED_CI";

/// The second account's name when the variable is unset.
pub const SECOND_ACCOUNT_DEFAULT: &str = "rdyother";

/// MAXIMUM_ALLOWED: ask the access check for everything the account would be granted.
const MAXIMUM_ALLOWED: u32 = 0x0200_0000;

/// FILE_LIST_DIRECTORY and FILE_READ_DATA share this bit.
pub const FILE_READ_BIT: u32 = 0x1;

/// The second account: its name and its SID.
#[derive(Debug, Clone)]
pub struct SecondAccount {
    pub name: String,
    pub sid: Sid,
}

/// Finds the second account. A missing account, a name that is not a user and the account
/// being this process's own are all errors that say what to provision.
#[allow(unsafe_code)]
pub fn second_account() -> io::Result<SecondAccount> {
    let name = std::env::var(SECOND_ACCOUNT_VAR).unwrap_or_else(|_| SECOND_ACCOUNT_DEFAULT.into());
    let missing = |why: String| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the second account `{name}` {why}: provision a standard local account outside \
                 the product and name it in {SECOND_ACCOUNT_VAR}"
            ),
        )
    };
    let wide_name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let mut sid = vec![0u8; 256];
    let mut domain = vec![0u16; 256];
    let (mut sid_len, mut domain_len, mut kind) = (sid.len() as u32, domain.len() as u32, 0);
    // SAFETY: the buffers are writable for the lengths given; `wide_name` is NUL-terminated.
    let found = unsafe {
        LookupAccountNameW(
            null(),
            wide_name.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        )
    };
    if found == 0 {
        return Err(missing(format!(
            "was not found ({})",
            io::Error::last_os_error()
        )));
    }
    if kind != SidTypeUser {
        return Err(missing("is not a user account".into()));
    }
    sid.truncate(sid_len as usize);
    let sid = Sid::parse(&sid)?;
    if sid.to_string() == own_sid()? {
        return Err(missing("is the account this test runs as".into()));
    }
    Ok(SecondAccount { name, sid })
}

/// A security descriptor the system allocated, freed on drop.
struct Descriptor(*mut c_void);

impl Drop for Descriptor {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: the pointer came from GetNamedSecurityInfoW and is freed once.
        unsafe { LocalFree(self.0) };
    }
}

/// What the system's access check grants `account` on the existing object `path`: the mask of
/// rights. 0 is no access. The object's owner, group and DACL are read from the object itself.
#[allow(unsafe_code)]
pub fn granted_access(account: &Sid, path: &Path) -> io::Result<u32> {
    let name = wide(path)?;
    let mut sd = null_mut();
    // SAFETY: `name` is NUL-terminated; only the descriptor out-pointer is used.
    let status = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let descriptor = Descriptor(sd);

    let mut manager = null_mut();
    // SAFETY: no callbacks and no name; `manager` is a writable handle slot.
    if unsafe {
        AuthzInitializeResourceManager(
            AUTHZ_RM_FLAG_NO_AUDIT,
            None,
            None,
            None,
            null(),
            &mut manager,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut context = null_mut();
    // SAFETY: the SID bytes live for the call; `context` is a writable handle slot.
    let made = unsafe {
        AuthzInitializeContextFromSid(
            0,
            account.as_bytes().as_ptr() as *mut c_void,
            manager,
            null_mut(),
            LUID {
                LowPart: 0,
                HighPart: 0,
            },
            null(),
            &mut context,
        )
    };
    let made_error = io::Error::last_os_error();
    if made == 0 {
        // SAFETY: the manager is valid and freed once.
        unsafe { AuthzFreeResourceManager(manager) };
        return Err(made_error);
    }

    let request = AUTHZ_ACCESS_REQUEST {
        DesiredAccess: MAXIMUM_ALLOWED,
        PrincipalSelfSid: null_mut(),
        ObjectTypeList: null_mut(),
        ObjectTypeListLength: 0,
        OptionalArguments: null_mut(),
    };
    let (mut granted, mut error) = (0u32, 0u32);
    let mut reply = AUTHZ_ACCESS_REPLY {
        ResultListLength: 1,
        GrantedAccessMask: &mut granted,
        SaclEvaluationResults: null_mut(),
        Error: &mut error,
    };
    // SAFETY: every pointer lives for the call; the descriptor is valid until `descriptor` drops.
    let checked = unsafe {
        AuthzAccessCheck(
            0,
            context,
            &request,
            null_mut(),
            descriptor.0,
            null_mut(),
            0,
            &mut reply,
            null_mut(),
        )
    };
    let checked_error = io::Error::last_os_error();
    // SAFETY: both handles are valid and freed once.
    unsafe {
        AuthzFreeContext(context);
        AuthzFreeResourceManager(manager);
    }
    if checked == 0 {
        return Err(checked_error);
    }
    if error == ERROR_ACCESS_DENIED {
        return Ok(0);
    }
    if error != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    Ok(granted)
}

/// Gives `account` the right to read and list `dir`, besides this account's full control, with
/// inheritance cut. A test calls this to show that [`granted_access`] can say yes, before it
/// judges a private object.
pub fn share_with(dir: &Path, account: &Sid) -> io::Result<()> {
    set_dacl(
        dir,
        &format!(
            "D:P(A;OICI;FA;;;{})(A;OICI;0x1200a9;;;{account})",
            own_sid()?
        ),
    )
}

/// What [`another_account_can_read`] proves on this platform: the system's own access check
/// for the second account. No process of that account runs.
pub const ANOTHER_ACCOUNT_PROOF: &str =
    "the system's access check (Authz) for the second account; no process of it runs";

/// Gives the second account the right to read and list the directory `dir` ([`share_with`]):
/// the control that shows [`another_account_can_read`] can say yes. A missing second account is
/// an error.
pub fn share_with_another_account(dir: &Path) -> io::Result<()> {
    share_with(dir, &second_account()?.sid)
}

/// Whether the second account may read `path` (or list it, for a directory), as the system's
/// access check grants it ([`granted_access`]). A missing second account is an error, never a
/// no. See [`ANOTHER_ACCOUNT_PROOF`].
pub fn another_account_can_read(path: &Path) -> io::Result<bool> {
    Ok(granted_access(&second_account()?.sid, path)? & FILE_READ_BIT != 0)
}

/// True when this process runs elevated (an administrator's full token).
#[allow(unsafe_code)]
pub fn is_elevated() -> io::Result<bool> {
    let mut token: HANDLE = null_mut();
    // SAFETY: the pseudo handle of this process is valid; `token` is a writable slot.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned = 0u32;
    // SAFETY: the token is open; the buffer holds one TOKEN_ELEVATION.
    let done = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    };
    let error = io::Error::last_os_error();
    // SAFETY: the token is closed once.
    unsafe { CloseHandle(token) };
    if done == 0 {
        return Err(error);
    }
    Ok(elevation.TokenIsElevated != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn wx24_second_account_cannot_open_private_objects() {
        // The hosted runner has no second account and names none. Only its workflow marks the run
        // (RDY_QA_HOSTED_CI=1), so the native gate stays mandatory. The line is not a
        // prerequisite skip: `--require-all` would refuse it, and the native gate holds the check.
        if std::env::var(HOSTED_CI_VAR).as_deref() == Ok("1")
            && std::env::var_os(SECOND_ACCOUNT_VAR).is_none()
        {
            let _ = std::io::Write::write_fmt(
                &mut std::io::stderr().lock(),
                format_args!(
                    "wx24_second_account_cannot_open_private_objects: not run on the hosted CI runner: no second account ({SECOND_ACCOUNT_VAR} is unset, {HOSTED_CI_VAR}=1); the native gate runs it\n"
                ),
            );
            return;
        }
        let other = second_account().expect("the second account is provisioned");
        let me = rdy_store::pio::current_account().expect("own account");
        assert_ne!(other.sid, me);
        // The test makes its own protected parent: a Temp folder that inherits a grant to other
        // accounts (a fixture root outside a profile) would give the second account the right
        // before the test shares anything, and the control below could not tell.
        let root = tempfile::tempdir().expect("tempdir");
        let tmp = root.path().join("fixture");
        rdy_store::pio::create_private_dir(&tmp).expect("private parent");

        // The control: a directory shared with the account is granted to it. Without this,
        // "no access" below could mean that the check cannot say yes.
        let shared = tmp.join("shared");
        fs::create_dir(&shared).expect("mkdir");
        let before = granted_access(&other.sid, &shared).expect("check before");
        share_with(&shared, &other.sid).expect("share");
        let after = granted_access(&other.sid, &shared).expect("check after");
        assert_eq!(before & FILE_READ_BIT, 0, "{} had no access", other.name);
        assert_eq!(
            after & FILE_READ_BIT,
            FILE_READ_BIT,
            "the setup granted the right"
        );
        assert_eq!(after & 0x2, 0, "read and list only, no add-file right");
        assert_eq!(
            granted_access(&me, &shared).expect("own check") & FILE_READ_BIT,
            FILE_READ_BIT
        );

        // The private objects this account makes: the other account has no right at all, and
        // this account can still open them.
        let private = tmp.join("private");
        rdy_store::pio::create_private_dir(&private).expect("private dir");
        let file = private.join("state");
        fs::write(&file, b"secret").expect("write");
        assert_eq!(fs::read(&file).expect("the owner reads"), b"secret");
        for object in [&private, &file] {
            assert_eq!(
                granted_access(&other.sid, object).expect("check"),
                0,
                "{} reaches {}",
                other.name,
                object.display()
            );
            assert_ne!(granted_access(&me, object).expect("own check"), 0);
        }

        // The check reads the real object: a path that is not there is an error.
        assert!(granted_access(&other.sid, &tmp.join("missing")).is_err());
    }
}
