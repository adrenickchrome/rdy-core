//! TASK-2307: on Linux with glibc the daemon fixes malloc's large-block threshold at start, so a
//! freed figure buffer of a few MiB does not move later ones into the heap arenas, which keep
//! freed memory. Its own test binary: the threshold is a setting of the whole process.

/// After the fix, a 1.5 MiB buffer allocated after a 4 MiB one was freed still gets its own
/// mapping. With glibc's moving threshold it came from the heap: the freed 4 MiB block had
/// raised the threshold to 4 MiB.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
fn a_freed_large_buffer_leaves_later_ones_mapped() {
    assert!(rdy_daemon::sys::fix_malloc_threshold());
    drop(std::hint::black_box(vec![1u8; 4 << 20]));
    let mapped = || rdy_daemon::sys::malloc_mapped_bytes().expect("glibc counters");
    let before = mapped();
    let held = std::hint::black_box(vec![1u8; 3 << 19]);
    let during = mapped();
    assert!(
        during >= before + held.len(),
        "the 1.5 MiB buffer is not mapped on its own: {before} -> {during} mapped bytes"
    );
    drop(held);
}

/// Elsewhere the call changes nothing and says so.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
#[test]
fn the_threshold_is_left_alone_off_glibc() {
    assert!(!rdy_daemon::sys::fix_malloc_threshold());
    assert_eq!(rdy_daemon::sys::malloc_mapped_bytes(), None);
}
