//! A project's start lock, `.rdy/run/start.lock`, and how a starter hands a start to the daemon it
//! launches. Whoever may open or replace the project's store, or start its daemon, holds a
//! [`StartGuard`] first. The lock order is start lock, then store lock. A nested open or upgrade
//! borrows the guard (`&StartGuard`) and never takes the lock again. Raw `Store` calls know no
//! project layout and take no guard.
//!
//! An operating system lock does not move to a child process. On Windows a handle the child
//! inherits loses the lock when the parent's handle closes, and the child can neither use nor
//! unlock the locked range. On Unix the child shares the lock, so either side's unlock frees it
//! for both (`probes/start-lock-transfer` shows each case). So the handoff releases the lock and
//! the child takes it again, and the reservation keeps the start the child's in between:
//!
//! 1. The starter holds the guard, launches the daemon and records it ([`StartGuard::reserve`]):
//!    the project's identity, the launched process's id and the digest of a one-use token. The
//!    token goes to that process over a private pipe, never on a command line or in the
//!    environment.
//! 2. The daemon claims the record ([`StartClaim::try_claim`]) with the token and its own process
//!    id, and holds the reservation file's lock from then on. The claim turns the record's digest
//!    into a mark, so the token is spent.
//! 3. The starter sees the claim ([`Reservation::state`]), lets the guard go and only then waits
//!    for the daemon to answer.
//! 4. The daemon takes the start lock ([`StartClaim::try_into_guard`]) and owns the start. Its
//!    guard's drop ends the reservation, then frees the lock.
//!
//! [`StartGuard::try_take`] refuses while a claim is held, so a rival that finds the start lock
//! free in step 3 gets no guard. The lock on the reservation file ends with its holder, so a
//! daemon's death frees it at once. A record whose lock is free, found by whoever holds the start
//! lock, is over: the starter that wrote it died or gave up, or the daemon that claimed it died.
//! The next taker clears it, and a child that comes late finds nothing to claim.
//!
//! Only the holder of the reservation file's lock reads or writes the file: Windows refuses a read
//! of a locked range through any other handle.
//!
//! The start lock file stays once it stands, with one exception: a load that made it removes it
//! as the load ends, while it holds it, so a load adds no run state to its target. A taker that
//! opened the file before that checks the name after it locks ([`FileLock::is_at`]), so a lock on
//! a removed file is no guard.

use crate::fileid::FileId;
use crate::layout::{RUN_DIR, START_LOCK_FILE};
use crate::pio;
use rdy_model::{Hash, RepoId};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The reservation's file in the run directory. It stays between starts, empty.
pub const RESERVATION_FILE: &str = "start.reservation";
/// The most bytes a reservation holds: a record is far shorter, whatever the platform's identity
/// text.
pub const MAX_RESERVATION_BYTES: usize = 256;

/// An exclusively locked file. Dropping it unlocks explicitly before the close: on Unix the lock
/// belongs to the open file description, which a child spawned by another thread shares until it
/// execs, so leaving the release to the close would keep the lock held after we are done.
pub struct FileLock(File);

impl FileLock {
    /// Opens `path` (create, read and write, private, never through a link; std opens it so that
    /// no child inherits it) and tries the lock once. `Ok(None)` when another holder has it.
    pub fn try_take(path: &Path) -> io::Result<Option<FileLock>> {
        let file = pio::open_no_follow(
            pio::private_file(
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false),
            ),
            path,
        )?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(e)) => return Err(e),
        }
        // guarded first, so an error below still unlocks explicitly
        let lock = FileLock(file);
        // a file left by an older run keeps its mode through open; set it exactly
        pio::make_private_file(&lock.0)?;
        Ok(Some(lock))
    }

    /// [`FileLock::try_take`] of a file that stands at `path` already: never creates it, and
    /// leaves its mode alone. `Ok(None)` when another holder has it; a missing file is
    /// `NotFound`.
    pub fn try_take_existing(path: &Path) -> io::Result<Option<FileLock>> {
        let file = pio::open_no_follow(OpenOptions::new().read(true).write(true), path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(FileLock(file))),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(e),
        }
    }

    /// Whether the held file is the file at `path` now. A load that made a start lock removes it
    /// when it fails: a rival that opened the file before that holds a lock on nothing.
    fn is_at(&self, path: &Path) -> bool {
        matches!(
            (FileId::of_file(&self.0), FileId::lstat(path)),
            (Ok(held), Ok(named)) if held == named
        )
    }

    /// Test hook: a duplicate of the descriptor, as a spawned child holds one until exec.
    #[doc(hidden)]
    pub fn share_descriptor_for_tests(&self) -> io::Result<File> {
        self.0.try_clone()
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// The one-use secret that ties a reservation to the launched process. The file holds only its
/// digest.
pub struct HandoffToken(String);

impl HandoffToken {
    /// The length of a token's text.
    pub const TEXT_LEN: usize = 32;

    /// A new random token.
    // a default token would be a random one, which `Default` should not hide
    #[allow(clippy::new_without_default)]
    pub fn new() -> HandoffToken {
        HandoffToken(RepoId::new().to_string())
    }

    /// The token whose text is `bytes`; `None` for anything but 32 lowercase hex digits.
    pub fn from_text(bytes: &[u8]) -> Option<HandoffToken> {
        let hex = |b: &u8| b.is_ascii_digit() || (b'a'..=b'f').contains(b);
        (bytes.len() == Self::TEXT_LEN && bytes.iter().all(hex))
            .then(|| HandoffToken(String::from_utf8_lossy(bytes).into_owned()))
    }

    /// The token's text, for the pipe to the launched process.
    pub fn text(&self) -> &[u8] {
        self.0.as_bytes()
    }

    fn digest(&self) -> String {
        Hash::of(self.0.as_bytes()).to_string()
    }
}

/// The record of a start that `pid` of `project` may claim with `token`.
fn unclaimed_record(project: FileId, pid: u32, token: &HandoffToken) -> String {
    format!("rdy-start 1 {project} {pid} {}\n", token.digest())
}

/// The record of a claimed start.
fn claimed_record(project: FileId, pid: u32) -> String {
    format!("rdy-start 1 {project} {pid} claimed\n")
}

/// The file's bytes, from its start, up to one past the bound: such a file matches no record.
fn read_record(file: &File) -> io::Result<Vec<u8>> {
    let mut reader = file;
    reader.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(MAX_RESERVATION_BYTES + 1);
    reader
        .take(MAX_RESERVATION_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Replaces the file's bytes with `record`. Run state outlives no crash that matters, so no sync.
fn write_record(file: &File, record: &str) -> io::Result<()> {
    debug_assert!(record.len() <= MAX_RESERVATION_BYTES);
    file.set_len(0)?;
    let mut writer = file;
    writer.seek(SeekFrom::Start(0))?;
    writer.write_all(record.as_bytes())
}

/// The run directory of the `.rdy` at `rdy_dir`, made private when it is missing. Anything else
/// standing at its name is refused.
fn run_dir(rdy_dir: &Path) -> io::Result<PathBuf> {
    let run = rdy_dir.join(RUN_DIR);
    match fs::symlink_metadata(&run) {
        Ok(m) if m.is_dir() => Ok(run),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", run.display()),
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            match pio::create_private_dir(&run) {
                Ok(()) => {}
                // another process made it first
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(run),
                Err(e) => return Err(e),
            }
            // the umask may have narrowed the mode; set it exactly
            pio::make_private_dir(&run)?;
            Ok(run)
        }
        Err(e) => Err(e),
    }
}

/// Whether no claim is held in the run directory `run`. Called by the holder of the start lock: a
/// record found with its lock free is over, and is cleared. A missing file is no claim.
fn no_claim_held(run: &Path) -> io::Result<bool> {
    let path = run.join(RESERVATION_FILE);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    let Some(lock) = FileLock::try_take(&path)? else {
        return Ok(false);
    };
    lock.0.set_len(0)?;
    Ok(true)
}

/// The start lock of one project, held. It names the project it guards and can only be had from
/// [`StartGuard::try_take`] or a claim. Its drop ends a claim it took over, then frees the lock.
pub struct StartGuard {
    // first: a reservation ends while the start lock still excludes every other starter
    _claim: Option<StartClaim>,
    lock: FileLock,
    rdy_dir: PathBuf,
    project: FileId,
}

impl StartGuard {
    /// One try at the start lock of the project whose `.rdy` directory is `rdy_dir`, which must
    /// exist: a guard makes the run directory, never the project. `Ok(None)` when another holder
    /// has the lock, or a launched daemon has claimed the start and has not taken the lock yet.
    pub fn try_take(rdy_dir: &Path) -> io::Result<Option<StartGuard>> {
        let project = FileId::lstat(rdy_dir)?;
        let run = run_dir(rdy_dir)?;
        let path = run.join(START_LOCK_FILE);
        let lock = match FileLock::try_take(&path) {
            Ok(Some(lock)) => lock,
            Ok(None) => return Ok(None),
            // a load that made the run directory removed it just now: try again
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        StartGuard::from_lock(lock, &path, &run, rdy_dir, project)
    }

    /// [`StartGuard::try_take`] that makes nothing: a project with no start lock file has no one
    /// to exclude yet, which is [`ExistingLock::Absent`]. For an open, which writes no run state.
    pub fn try_take_existing(rdy_dir: &Path) -> io::Result<ExistingLock> {
        let run = rdy_dir.join(RUN_DIR);
        let path = run.join(START_LOCK_FILE);
        let absent = |e: io::Error| match e.kind() {
            io::ErrorKind::NotFound => Ok(ExistingLock::Absent),
            _ => Err(e),
        };
        let project = match FileId::lstat(rdy_dir) {
            Ok(id) => id,
            Err(e) => return absent(e),
        };
        let lock = match FileLock::try_take_existing(&path) {
            Ok(Some(lock)) => lock,
            Ok(None) => return Ok(ExistingLock::Busy),
            Err(e) => return absent(e),
        };
        Ok(
            match StartGuard::from_lock(lock, &path, &run, rdy_dir, project)? {
                Some(guard) => ExistingLock::Held(guard),
                None => ExistingLock::Busy,
            },
        )
    }

    /// The guard of `lock`, just taken on the file `path` in the run directory `run`. `Ok(None)`
    /// when that file left its name (nothing excludes anyone through it) or a launched daemon
    /// holds a claim.
    fn from_lock(
        lock: FileLock,
        path: &Path,
        run: &Path,
        rdy_dir: &Path,
        project: FileId,
    ) -> io::Result<Option<StartGuard>> {
        if !lock.is_at(path) || !no_claim_held(run)? {
            return Ok(None);
        }
        Ok(Some(StartGuard {
            _claim: None,
            lock,
            rdy_dir: rdy_dir.to_path_buf(),
            project,
        }))
    }

    /// Whether this is the start lock of the project whose `.rdy` directory is `rdy_dir` now: the
    /// same directory, and the held file is the file at the lock's name. A `.rdy` renamed after
    /// the guard was taken keeps its guard.
    pub fn guards(&self, rdy_dir: &Path) -> bool {
        let at_name = rdy_dir.join(RUN_DIR).join(START_LOCK_FILE);
        matches!(
            (FileId::lstat(rdy_dir), FileId::of_file(&self.lock.0), FileId::lstat(&at_name)),
            (Ok(dir), Ok(held), Ok(named)) if dir == self.project && held == named
        )
    }

    /// The project this guard names: the identity of its `.rdy`.
    pub fn project(&self) -> FileId {
        self.project
    }

    /// Records the start of the launched process `pid`, which `token` lets claim it. `Ok(None)`
    /// while another process holds the reservation file for a moment: a child that a dead starter
    /// launched, looking at the record. Try again.
    pub fn reserve(&self, pid: u32, token: &HandoffToken) -> io::Result<Option<Reservation<'_>>> {
        let path = self.rdy_dir.join(RUN_DIR).join(RESERVATION_FILE);
        let Some(lock) = FileLock::try_take(&path)? else {
            return Ok(None);
        };
        let record = unclaimed_record(self.project, pid, token);
        write_record(&lock.0, &record)?;
        Ok(Some(Reservation {
            _guard: self,
            path,
            record,
        }))
    }
}

/// What one try at a start lock that must already exist found.
pub enum ExistingLock {
    /// No start lock file stands there.
    Absent,
    /// Another holder has it, or a launched daemon holds a claim.
    Busy,
    /// It is held now.
    Held(StartGuard),
}

/// What has become of a reservation, as its writer sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReservationState {
    /// The launched process has not claimed it.
    Unclaimed,
    /// A process holds the reservation file: the daemon owns the start from here.
    Claimed,
    /// It holds no longer this record: a claim came and went.
    Ended,
}

/// A start recorded under a guard, as the starter that wrote it holds it.
pub struct Reservation<'g> {
    _guard: &'g StartGuard,
    path: PathBuf,
    record: String,
}

impl Reservation<'_> {
    /// What has become of this reservation.
    pub fn state(&self) -> io::Result<ReservationState> {
        let Some(lock) = FileLock::try_take(&self.path)? else {
            return Ok(ReservationState::Claimed);
        };
        Ok(if read_record(&lock.0)? == self.record.as_bytes() {
            ReservationState::Unclaimed
        } else {
            ReservationState::Ended
        })
    }

    /// Gives this start up (a dead child, a timeout): clears the record while it is still this
    /// one, so the launched process finds nothing to claim. False, with nothing touched, when it
    /// is no longer this start's or the daemon has claimed it: that claim ends with its holder.
    pub fn withdraw(self) -> io::Result<bool> {
        let Some(lock) = FileLock::try_take(&self.path)? else {
            return Ok(false);
        };
        if read_record(&lock.0)? != self.record.as_bytes() {
            return Ok(false);
        }
        lock.0.set_len(0)?;
        Ok(true)
    }
}

/// What a try at a claim found.
pub enum ClaimOutcome {
    /// The start is this process's.
    Claimed(StartClaim),
    /// Another process holds the reservation file for now. Try again.
    Busy,
    /// No record for this token, this process and this project: the starter gave up, or another
    /// start won. Never open the store on this.
    NotOurs,
}

/// A launched daemon's claim on its start: the reservation file's lock, held. The claim's drop
/// clears the record.
pub struct StartClaim {
    lock: FileLock,
    rdy_dir: PathBuf,
    project: FileId,
}

impl StartClaim {
    /// One try at claiming the start of the project whose `.rdy` directory is `rdy_dir` for the
    /// process `pid` with `token`. Refused, nothing is written.
    pub fn try_claim(rdy_dir: &Path, token: &HandoffToken, pid: u32) -> io::Result<ClaimOutcome> {
        let project = FileId::lstat(rdy_dir)?;
        let path = run_dir(rdy_dir)?.join(RESERVATION_FILE);
        let Some(lock) = FileLock::try_take(&path)? else {
            return Ok(ClaimOutcome::Busy);
        };
        if read_record(&lock.0)? != unclaimed_record(project, pid, token).as_bytes() {
            return Ok(ClaimOutcome::NotOurs);
        }
        write_record(&lock.0, &claimed_record(project, pid))?;
        Ok(ClaimOutcome::Claimed(StartClaim {
            lock,
            rdy_dir: rdy_dir.to_path_buf(),
            project,
        }))
    }

    /// One try at the start lock for this claim. The reservation does not stop its own holder.
    /// `Ok(Err(claim))` while another process has the lock: its starter has not let go yet, or a
    /// rival looks at the reservation for a moment. Try again.
    pub fn try_into_guard(self) -> io::Result<Result<StartGuard, StartClaim>> {
        let path = self.rdy_dir.join(RUN_DIR).join(START_LOCK_FILE);
        let Some(lock) = FileLock::try_take(&path)? else {
            return Ok(Err(self));
        };
        if !lock.is_at(&path) {
            return Ok(Err(self));
        }
        Ok(Ok(StartGuard {
            rdy_dir: self.rdy_dir.clone(),
            project: self.project,
            _claim: Some(self),
            lock,
        }))
    }
}

impl Drop for StartClaim {
    fn drop(&mut self) {
        // a file left unclear reads as over to the next taker, once this lock is gone
        let _ = self.lock.0.set_len(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wx8a_records_fit_the_bound_on_every_platform() {
        let token = HandoffToken::new();
        let longest = [
            FileId::Unix {
                dev: u64::MAX,
                ino: u64::MAX,
            },
            FileId::Windows {
                volume: u64::MAX,
                id: [0xff; 16],
            },
        ];
        for id in longest {
            assert!(unclaimed_record(id, u32::MAX, &token).len() <= MAX_RESERVATION_BYTES);
            assert!(claimed_record(id, u32::MAX).len() <= MAX_RESERVATION_BYTES);
        }
    }

    #[test]
    fn wx8b_a_lock_on_a_removed_file_is_at_no_name() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("start.lock");
        let lock = FileLock::try_take(&path).unwrap().unwrap();
        assert!(lock.is_at(&path));
        // a rival's handle after a removal: the name is gone
        fs::remove_file(&path).unwrap();
        assert!(!lock.is_at(&path));
        // or another file stands at it
        let other = FileLock::try_take(&path).unwrap().unwrap();
        assert!(!lock.is_at(&path));
        assert!(other.is_at(&path));
    }

    #[test]
    fn wx8a_a_token_is_32_lowercase_hex_digits() {
        let token = HandoffToken::new();
        assert_eq!(token.text().len(), HandoffToken::TEXT_LEN);
        assert!(HandoffToken::from_text(token.text()).is_some());
        for bad in [&b""[..], b"short", &[b'A'; 32], &[b'g'; 32], &[b'0'; 33]] {
            assert!(HandoffToken::from_text(bad).is_none());
        }
    }
}
