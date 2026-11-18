//! Shared test-support helpers for the ACOS workspace.
//!
//! The `ACOS_REQUIRE_FIXTURES` gate (HONESTY_AUDIT #11, W67A #7) must be
//! *global*: a fixture-dependent differential test that prints `SKIP` and
//! passes when a gitignored artifact is absent makes CI green without any
//! evidence. Every such test calls [`have_fixture`]; when the environment
//! variable is set and the artifact is missing, it panics instead of skipping.
//!
//! `tools/diff-dexprogs.sh` and the bench scripts already export
//! `ACOS_REQUIRE_FIXTURES=1`, so their runs now hard-fail on a missing fixture.

use std::path::{Path, PathBuf};
use std::sync::Once;

pub mod prereq;

pub use prereq::{
    art_inputs, art_sysroot, art_warm_data, require_art, require_prereq, skip_allowed,
    skipped_count, ArtInputs, ALLOW_ANY_SKIP_ENV, ALLOW_SKIP_ENV, WARM_DATA_ENV,
};

/// The workspace root (the parent of `crates/`).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Whether `ACOS_REQUIRE_FIXTURES` is set in the environment.
pub fn fixtures_required() -> bool {
    std::env::var_os("ACOS_REQUIRE_FIXTURES").is_some()
}

/// Whether the workspace-relative `rel` artifact exists.
///
/// When it is missing *and* `ACOS_REQUIRE_FIXTURES` is set, this panics: CI
/// must build the fixture (`tools/build-fixtures.sh`) rather than skip. When
/// the variable is not set it returns `false` so a developer checkout still
/// builds and its tests loudly skip.
pub fn have_fixture(rel: &str) -> bool {
    let path = repo_root().join(rel);
    if path.exists() {
        return true;
    }
    if fixtures_required() {
        panic!(
            "ACOS_REQUIRE_FIXTURES is set but {rel} is missing; build the fixtures \
             (tools/build-fixtures.sh) instead of skipping"
        );
    }
    false
}

/// Hard-fail under `ACOS_REQUIRE_FIXTURES` if any listed artifact is missing;
/// otherwise a no-op. Suitable for a dedicated CI gate test.
pub fn require_all(rels: &[&str]) {
    if !fixtures_required() {
        return;
    }
    let root = repo_root();
    for rel in rels {
        assert!(
            root.join(rel).exists(),
            "ACOS_REQUIRE_FIXTURES is set but {rel} is missing; CI must build the \
             fixtures instead of skipping"
        );
    }
}

// ---------------------------------------------------------------------------
// Short roots for test scratch that holds a Unix socket (SUN_LEN, class rule)
// ---------------------------------------------------------------------------

/// Bytes a pathname `AF_UNIX` address may use on Linux: `sun_path` is 108 bytes
/// including the terminating NUL, so a socket path may be at most this long.
pub const SUN_PATH_MAX: usize = 107;

/// The longest pid a Linux host can print (`/proc/sys/kernel/pid_max` is capped
/// at 4194304), used to size the scratch budget below.
const PID_MAX_LEN: usize = 7;

/// The deepest name the viewer tests put below their scratch root. The budget
/// is derived from it, so a root that "looks short" cannot still overflow.
const DEEPEST_BELOW_ROOT: &str = "-frames-and-events-stream/producer.sock";

/// Longest accepted test scratch root: everything below it
/// (`/<pid>-frames-and-events-stream/producer.sock`) must still fit
/// [`SUN_PATH_MAX`].
pub const SCRATCH_ROOT_BUDGET: usize = SUN_PATH_MAX - 1 - PID_MAX_LEN - DEEPEST_BELOW_ROOT.len();

static NARROWED: Once = Once::new();

/// The root for test scratch that has to hold a Unix socket.
///
/// `sun_path` is 108 bytes with the NUL and a checkout path is not something a
/// test controls (agent worktrees, CI job dirs are long), so a test that binds
/// `registry/v.sock` under `<checkout>/.cache/...` used to fail with "path must
/// be shorter than SUN_LEN" instead of testing what it asserts. There is one
/// rule for all of them:
///
/// 1. `$env_var` when it fits [`SCRATCH_ROOT_BUDGET`] - a caller's explicit
///    choice is honoured;
/// 2. the first of `candidates` (the checkout's own `.cache/...`) that fits;
/// 3. `/dev/shm/<fallback_name>`, a fixed short RAM-backed path (these tests
///    write a handful of bytes, never real artifacts);
/// 4. the system temp dir as the last resort.
///
/// A too-long `$env_var` is **not** an error: it is reported once on stderr with
/// its length and the budget, and the rule falls through to a short root, so a
/// caller's long scratch directory degrades to a short one instead of failing
/// tests that have nothing to do with it.
pub fn short_scratch_root(env_var: &str, candidates: &[PathBuf], fallback_name: &str) -> PathBuf {
    let from_env = std::env::var_os(env_var).map(PathBuf::from);
    let (root, ignored) = pick_scratch_root(from_env.as_deref(), candidates, fallback_name);
    if let Some(ignored) = ignored {
        let len = ignored.as_os_str().as_encoded_bytes().len();
        NARROWED.call_once(|| {
            eprintln!(
                "{env_var} is {len} bytes ({}), which leaves no room for a Unix socket \
                 path (sun_path is {} bytes with the NUL, at most {} bytes for a root); \
                 test scratch goes to {} instead",
                ignored.display(),
                SUN_PATH_MAX + 1,
                SCRATCH_ROOT_BUDGET,
                root.display()
            );
        });
    }
    root
}

/// The decision [`short_scratch_root`] makes, without touching the environment
/// (so it is testable in a parallel test run): returns the root and, when the
/// requested one was too long, the value that was not used.
pub fn pick_scratch_root(
    requested: Option<&Path>,
    candidates: &[PathBuf],
    fallback_name: &str,
) -> (PathBuf, Option<PathBuf>) {
    let fits = |dir: &Path| dir.as_os_str().as_encoded_bytes().len() <= SCRATCH_ROOT_BUDGET;
    if let Some(requested) = requested {
        if fits(requested) {
            return (requested.to_path_buf(), None);
        }
    }
    let root = candidates
        .iter()
        .find(|candidate| fits(candidate))
        .cloned()
        .unwrap_or_else(|| {
            let shm = Path::new("/dev/shm");
            if shm.is_dir() {
                shm.join(fallback_name)
            } else {
                std::env::temp_dir().join(fallback_name)
            }
        });
    (
        root,
        requested.filter(|dir| !fits(dir)).map(Path::to_path_buf),
    )
}

/// The scratch directory a test publishes evidence into.
///
/// Tests used to write their evidence straight into the checkout's tracked
/// `results/` tree (`results/flutter/`, `results/rn/`, `results/render/`, ...),
/// so a suite run left `git status` dirty and could overwrite a committed frame,
/// coverage table or probe JSON. One rule for that class: `$ACOS_TEST_ARTIFACTS`
/// when set, otherwise `<checkout>/.cache/test-artifacts/<name>`. Never the
/// `results/` tree, which is evidence a human curated.
///
/// `repo_root` is the caller's checkout (each test crate has its own
/// `repo_root()`); `name` is the per-suite subdirectory (`flutter`, `rn`, ...).
pub fn test_artifacts_dir_from(repo_root: &Path, requested: Option<&Path>, name: &str) -> PathBuf {
    requested
        .map(Path::to_path_buf)
        .unwrap_or_else(|| repo_root.join(".cache").join("test-artifacts"))
        .join(name)
}

/// [`test_artifacts_dir_from`] reading `$ACOS_TEST_ARTIFACTS`, relative to this
/// workspace's checkout.
pub fn test_artifacts_dir(name: &str) -> PathBuf {
    let requested = std::env::var_os("ACOS_TEST_ARTIFACTS").map(PathBuf::from);
    test_artifacts_dir_from(&repo_root(), requested.as_deref(), name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests that publish evidence wrote into the checkout's tracked `results/`
    /// tree, so a suite run left `git status` dirty and could overwrite a
    /// committed frame or coverage file. One rule for that class: a scratch
    /// root, never the checkout.
    #[test]
    fn test_artifacts_go_to_a_scratch_root_and_never_to_the_checkout_results_tree() {
        let checkout = Path::new("/home/agent/wt");
        let dir = test_artifacts_dir_from(checkout, Some(Path::new("/scratch/artifacts")), "flutter");
        assert_eq!(dir, Path::new("/scratch/artifacts/flutter"));
        let dir = test_artifacts_dir_from(checkout, None, "rn");
        assert_eq!(dir, checkout.join(".cache/test-artifacts/rn"));
        assert!(
            !dir.starts_with(checkout.join("results")),
            "test artifacts must not land in the tracked results/ tree: {}",
            dir.display()
        );
    }

    #[test]
    fn a_root_that_cannot_hold_a_socket_falls_through_to_a_short_one() {
        // The SETTLE5 failure, as a unit: a 114-byte root plus
        // `/<pid>-frames-and-events-stream/producer.sock` is 161 bytes.
        let long = PathBuf::from(format!("/tmp/{}", "x".repeat(70)));
        assert!(long.as_os_str().as_encoded_bytes().len() > SCRATCH_ROOT_BUDGET);
        let checkout = PathBuf::from("/home/agent/wt/.cache/lv");
        let (root, ignored) = pick_scratch_root(Some(&long), &[checkout.clone()], "acos-lv-tests");
        assert_eq!(root, checkout);
        assert_eq!(ignored.as_deref(), Some(long.as_path()));
    }

    #[test]
    fn the_budget_is_derived_from_the_deepest_name_and_not_a_round_number() {
        // A root of exactly the budget still fits the deepest socket name; one
        // byte more does not. (The old 72-byte budget passed its own assert and
        // still overflowed: 67 bytes of root made a 114-byte socket path.)
        let deepest = |root: &str| {
            // `<root>/<pid>-frames-and-events-stream/producer.sock`
            format!("{root}/{}{DEEPEST_BELOW_ROOT}", 9_999_999)
                .as_bytes()
                .len()
        };
        let exact = "r".repeat(SCRATCH_ROOT_BUDGET);
        assert!(
            deepest(&exact) <= SUN_PATH_MAX,
            "a root of exactly the budget must fit: {} bytes",
            deepest(&exact)
        );
        assert!(deepest(&format!("r{}", exact)) > SUN_PATH_MAX);
    }

    #[test]
    fn a_short_requested_root_is_honoured() {
        let short = PathBuf::from("/tmp/lv");
        let checkout = PathBuf::from("/home/agent/wt/.cache/lv");
        let (root, ignored) = pick_scratch_root(Some(&short), &[checkout], "acos-lv-tests");
        assert_eq!(root, short);
        assert!(ignored.is_none());
    }

    #[test]
    fn no_usable_candidate_falls_back_to_a_short_ram_disk_root() {
        let long = PathBuf::from(format!("/tmp/{}", "y".repeat(70)));
        let (root, ignored) = pick_scratch_root(Some(&long), &[long.clone()], "acos-lv-tests");
        assert!(root.as_os_str().as_encoded_bytes().len() <= SCRATCH_ROOT_BUDGET);
        assert!(root.ends_with("acos-lv-tests"));
        assert_eq!(ignored.as_deref(), Some(long.as_path()));
    }
}
