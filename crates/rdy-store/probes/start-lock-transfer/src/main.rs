//! What an operating system's file lock does across a handle clone and a child process. Each case
//! prints `case: result`. No case asserts: the output is the evidence that a plan cites.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// Opens `path` for reading and writing, creating it.
fn open(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .expect("open")
}

/// Tries the lock through a new handle: whether a new opener can take it now.
fn free(path: &Path) -> &'static str {
    let f = open(path);
    match f.try_lock() {
        Ok(()) => {
            f.unlock().expect("unlock");
            "free"
        }
        Err(TryLockError::WouldBlock) => "held",
        Err(TryLockError::Error(_)) => "error",
    }
}

/// `f`, the child's stdin, as a file.
#[cfg(unix)]
fn stdin_file() -> File {
    use std::os::fd::FromRawFd;
    // SAFETY: the parent gave this process a file as its stdin, and nothing else uses it here.
    unsafe { File::from_raw_fd(0) }
}

/// `f`, the child's stdin, as a file.
#[cfg(windows)]
fn stdin_file() -> File {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    // SAFETY: the parent gave this process a file as its stdin, and nothing else uses it here.
    unsafe { File::from_raw_handle(std::io::stdin().as_raw_handle()) }
}

/// A child of this program in `mode`, with `stdin` as its stdin and a pipe on its stdout.
fn child(mode: &str, path: &Path, stdin: Stdio) -> Child {
    Command::new(std::env::current_exe().expect("exe"))
        .arg(mode)
        .arg(path)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn")
}

/// Reads one line from `c`'s stdout.
fn line(c: &mut Child) -> String {
    let out = c.stdout.as_mut().expect("stdout");
    let mut text = Vec::new();
    let mut b = [0u8; 1];
    while out.read(&mut b).expect("read") == 1 && b[0] != b'\n' {
        text.push(b[0]);
    }
    String::from_utf8_lossy(&text).trim().to_string()
}

fn say(text: &str) {
    println!("{text}");
    std::io::stdout().flush().expect("flush");
}

/// Child modes. Each one reports over stdout, then waits for its stdin's owner to end it.
fn child_mode(mode: &str, path: &Path) {
    match mode {
        // takes the lock through a new handle and holds it until it is killed
        "hold" => {
            let f = open(path);
            f.lock().expect("lock");
            say("locked");
            loop {
                std::thread::park();
            }
        }
        // holds the inherited handle and does nothing else until it is killed
        "keep" => {
            let _f = stdin_file();
            say("inherited");
            loop {
                std::thread::park();
            }
        }
        // holds the inherited handle, waits for the go file, says what it can do through the
        // handle, and stays
        "inherit" => {
            let f = stdin_file();
            say("inherited");
            let go = path.with_extension("go");
            while !go.exists() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let mut buf = [0u8; 1];
            let read = (&f).seek(SeekFrom::Start(0)).and_then(|_| (&f).read(&mut buf));
            say(&format!("read through the inherited handle: {read:?}"));
            say(&format!(
                "try_lock through the inherited handle: {:?}",
                f.try_lock().map_err(|e| e.to_string())
            ));
            say(&format!(
                "unlock through the inherited handle: {:?}",
                f.unlock().map_err(|e| e.to_string())
            ));
            say("done");
            loop {
                std::thread::park();
            }
        }
        // reads a token from its stdin, the private pipe of a handoff
        "token" => {
            let mut text = [0u8; 32];
            match std::io::stdin().read_exact(&mut text) {
                Ok(()) => say(&format!("read {}", String::from_utf8_lossy(&text))),
                Err(e) => say(&format!("no token: {:?}", e.kind())),
            }
            std::process::exit(0);
        }
        other => panic!("unknown mode {other}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 3 {
        child_mode(&args[1], &PathBuf::from(&args[2]));
        return;
    }
    let dir = std::env::temp_dir().join(format!("rdy-lock-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("start.lock");
    println!("os: {} {}", std::env::consts::OS, std::env::consts::ARCH);

    // 1. two handles of one process exclude each other
    {
        let a = open(&path);
        a.try_lock().expect("first lock");
        println!("1 second handle in this process, while the first holds it: {}", free(&path));
        a.unlock().expect("unlock");
    }

    // 2. a clone in this process, once the handle that took the lock is closed
    {
        let a = open(&path);
        a.try_lock().expect("first lock");
        let clone = a.try_clone().expect("clone");
        println!("2a with both handles open: {}", free(&path));
        drop(a);
        println!("2b the locking handle closed, its clone open: {}", free(&path));
        drop(clone);
        println!("2c the clone closed too: {}", free(&path));
    }

    // 3. a handle a child inherits, while the parent holds the lock and after it closes its own
    {
        let a = open(&path);
        a.try_lock().expect("first lock");
        let mut c = child("inherit", &path, Stdio::from(a.try_clone().expect("clone")));
        assert_eq!(line(&mut c), "inherited");
        println!("3a parent and child hold handles: {}", free(&path));
        std::fs::write(path.with_extension("go"), b"").expect("go");
        println!("3b {}", line(&mut c));
        println!("3c {}", line(&mut c));
        println!("3d {}", line(&mut c));
        assert_eq!(line(&mut c), "done");
        println!("3e after the child's try_lock and unlock: {}", free(&path));
        c.kill().expect("kill");
        c.wait().expect("wait");
        drop(a);
    }

    // 4. the parent gives up its handle without an unlock while the child keeps its inherited one
    {
        let a = open(&path);
        a.try_lock().expect("first lock");
        let cloned = a.try_clone().expect("clone");
        let mut c = child("keep", &path, Stdio::from(cloned));
        assert_eq!(line(&mut c), "inherited");
        drop(a);
        println!("4a parent's handle closed, child's open: {}", free(&path));
        c.kill().expect("kill");
        c.wait().expect("wait");
        println!("4b child gone too: {}", free(&path));
    }

    // 5. the death of a process that holds the lock releases it
    {
        let mut c = child("hold", &path, Stdio::null());
        assert_eq!(line(&mut c), "locked");
        println!("5a another process holds it: {}", free(&path));
        c.kill().expect("kill");
        c.wait().expect("wait");
        println!("5b that process killed: {}", free(&path));
    }

    // 6. what another handle can read of a locked file
    {
        let a = open(&path);
        (&a).write_all(b"x").expect("write");
        a.try_lock().expect("first lock");
        let mut other = open(&path);
        let mut buf = [0u8; 1];
        println!(
            "6a a read through another handle of a locked file: {:?}",
            other.read(&mut buf).map_err(|e| e.to_string())
        );
        (&a).seek(SeekFrom::Start(0)).expect("seek");
        let mut mine = [0u8; 1];
        println!(
            "6b a read through the locking handle: {:?}",
            (&a).read(&mut mine).map_err(|e| e.to_string())
        );
        a.unlock().expect("unlock");
    }

    // 7. a pipe as a child's stdin: what a handoff's token travels on
    {
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        let mut c = child("token", &path, Stdio::from(reader));
        writer.write_all(&[b'a'; 32]).expect("write");
        drop(writer);
        println!("7a a token sent after the spawn: {}", line(&mut c));
        c.wait().expect("wait");
        let (reader, writer) = std::io::pipe().expect("pipe");
        let mut c = child("token", &path, Stdio::from(reader));
        drop(writer);
        println!("7b the sender gone with no token: {}", line(&mut c));
        c.wait().expect("wait");
        let mut c = child("token", &path, Stdio::null());
        println!("7c a null stdin: {}", line(&mut c));
        c.wait().expect("wait");
    }

    let _ = std::fs::remove_dir_all(&dir);
}
