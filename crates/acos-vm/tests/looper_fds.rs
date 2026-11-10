//! Acceptance (d): 10k `MessageQueue` native looper create/destroy cycles leak
//! no file descriptors.
//!
//! This is the only test in its binary because it compares the process-wide
//! `/proc/self/fd` count, which parallel test threads could perturb.

use acos_vm::natives::looper::{create_looper, destroy_looper};

fn fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .map(|rd| rd.count())
        .unwrap_or(0)
}

#[test]
fn looper_create_destroy_leaks_no_fds() {
    // Warm up: the first looper lazily creates the process `epoll` fd, which is
    // intentionally long lived and must not count as a leak.
    let warm = create_looper().expect("create looper");
    destroy_looper(warm.id());
    drop(warm);

    let before = fd_count();
    for _ in 0..10_000 {
        let h = create_looper().expect("create looper");
        destroy_looper(h.id());
        drop(h);
    }
    let after = fd_count();
    assert_eq!(
        before, after,
        "fd count changed after 10k looper create/destroy cycles"
    );
}
