//! W43: the harness-failure classification helpers in `acos_art::launch`.
//!
//! The 2026-11-12 ENOSPC window showed the difference between an app result and
//! a host condition: `INTERNAL: acos-art session exited`, `INTERNAL: cannot
//! write the C1 capture: No space left on device` and a caller-side panic with
//! no stderr at all. A full scratch disk must be a *structured, retryable*
//! `HOST_DISK_FULL`; these tests pin the decision, including the negative cases
//! (an unreadable free-space answer must never refuse a run).

use std::path::Path;

use acos_art::launch::{disk_free_bytes, disk_full_for_run, is_disk_full};

#[test]
fn free_space_is_readable_on_a_real_directory() {
    let free = disk_free_bytes(Path::new(".")).expect("statvfs on the repo root works");
    assert!(free > 0, "a live filesystem reports free bytes, got {free}");
}

#[test]
fn free_space_of_a_missing_path_is_none_not_zero() {
    // A missing path must not be mistaken for "no space": `None` never refuses.
    assert!(disk_free_bytes(Path::new("/nonexistent-w43-path/xyz")).is_none());
    assert!(!disk_full_for_run(None, u64::MAX));
}

#[test]
fn enospc_and_edquot_are_host_disk_full_and_other_errors_are_not() {
    let enospc = std::io::Error::from_raw_os_error(28);
    let edquot = std::io::Error::from_raw_os_error(122);
    let enoent = std::io::Error::from_raw_os_error(2);
    let epipe = std::io::Error::from_raw_os_error(32);
    assert!(
        is_disk_full(&enospc),
        "ENOSPC is the scratch disk being full"
    );
    assert!(is_disk_full(&edquot), "EDQUOT is the same host condition");
    assert!(!is_disk_full(&enoent));
    assert!(!is_disk_full(&epipe));
}

#[test]
fn the_preflight_refuses_only_below_the_threshold() {
    let min = 512 * 1024 * 1024;
    assert!(
        disk_full_for_run(Some(min - 1), min),
        "one byte short is a refusal"
    );
    assert!(
        !disk_full_for_run(Some(min), min),
        "exactly the threshold is allowed"
    );
    assert!(!disk_full_for_run(Some(min * 4), min));
    assert!(
        !disk_full_for_run(None, min),
        "unknown free space is not a refusal"
    );
}
