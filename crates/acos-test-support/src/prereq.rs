//! A skipped test must never look like a pass (HYG2).
//!
//! The workspace had ~150 `eprintln!("SKIP …"); return;` sites. A `cargo test`
//! run then reported `test result: ok` while the test asserted nothing, and
//! BACKSETTLE's `results/handoff/BACKSETTLE/suite3.log` is the documented
//! false-green: `run-exclusive.sh` exports a *fresh, empty* `ACOS_ART_DATA`, so
//! the six ART-booting `crates/acos-art/tests/cleanup.rs` cases printed
//! `SKIP cleanup test: no cached guest dex` and the suite was green without
//! ever starting a guest.
//!
//! One rule for the whole class: [`require_prereq`] **fails** the test when a
//! prerequisite is missing, naming it, the reason and the command that provides
//! it. A counted skip exists only behind an explicit opt-out
//! ([`ALLOW_SKIP_ENV`]), and it is *loud*: the reason is printed with the
//! running skip count and every test process prints a `SKIP SUMMARY` line on
//! exit, so a log with skips cannot be mistaken for a log without them.
//!
//! The ART prerequisites themselves also get one rule ([`require_art`]): three
//! test files each carried their own copy of "sysroot has `system/apex`, the
//! warm data has `app/guest/classes.dex`, else skip", and each resolved the
//! warm data differently. [`art_inputs`] resolves them the same way for
//! everyone, preferring the *warm cache* variable ([`WARM_DATA_ENV`], exported
//! by `tools/art-runtime/run-exclusive.sh`) over the per-invocation scratch
//! `ACOS_ART_DATA`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;

/// The explicit opt-out: `ACOS_ALLOW_ART_SKIP=1` turns a missing prerequisite
/// into a counted skip instead of a failure.
pub const ALLOW_SKIP_ENV: &str = "ACOS_ALLOW_ART_SKIP";

/// A second spelling of the same opt-out, for a suite whose prerequisite is not
/// ART (a host tool, a fixture).
pub const ALLOW_ANY_SKIP_ENV: &str = "ACOS_ALLOW_TEST_SKIP";

/// The warm ART cache a test may use as a prerequisite: a `/data` tree with
/// `app/guest/classes.dex`, produced once by a real `acos-art render-app` run.
///
/// `ACOS_ART_DATA` is the *per-invocation scratch* directory (`run-exclusive.sh`
/// creates an empty one), so it is a bad name for "the warm cache": the whole
/// false-green above came from conflating the two. `run-exclusive.sh` exports
/// this variable inside the lock.
pub const WARM_DATA_ENV: &str = "ACOS_ART_WARM_DATA";

/// The ART sysroot (`tools/art-sysroot.sh`).
pub const SYSROOT_ENV: &str = "ACOS_ART_SYSROOT";

static SKIPPED: AtomicUsize = AtomicUsize::new(0);
static SUMMARY: Once = Once::new();

/// Whether a missing prerequisite may be a counted skip.
#[must_use]
pub fn skip_allowed() -> bool {
    [ALLOW_SKIP_ENV, ALLOW_ANY_SKIP_ENV]
        .into_iter()
        .any(skip_allowed_by)
}

/// The opt-out variable that is set (the name is printed, so a log says which
/// one allowed the skip), or [`ALLOW_SKIP_ENV`] when none is.
#[must_use]
pub fn active_opt_out() -> &'static str {
    [ALLOW_SKIP_ENV, ALLOW_ANY_SKIP_ENV]
        .into_iter()
        .find(|name| skip_allowed_by(name))
        .unwrap_or(ALLOW_SKIP_ENV)
}

fn skip_allowed_by(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        let value = value.trim().to_ascii_lowercase();
        value == "1" || value == "true" || value == "yes"
    })
}

/// How many prerequisites this process skipped (always 0 on the default path).
#[must_use]
pub fn skipped_count() -> usize {
    SKIPPED.load(Ordering::SeqCst)
}

/// The one rule for a missing test prerequisite.
///
/// * `present` — the prerequisite is there: `Some(())`, nothing is printed.
/// * missing and **no** opt-out: panics. A test that cannot assert anything must
///   fail, not pass.
/// * missing with [`ALLOW_SKIP_ENV`] set: records and prints the skip (with the
///   running count) and returns `None`, so the caller returns early.
///
/// Call it as `if require_prereq(cond, "cleanup test", "reason").is_none() { return; }`.
pub fn require_prereq(present: bool, test: &str, reason: &str) -> Option<()> {
    if present {
        return Some(());
    }
    if skip_allowed() {
        let count = SKIPPED.fetch_add(1, Ordering::SeqCst) + 1;
        install_summary();
        let opt_out = active_opt_out();
        eprintln!("SKIP {test}: {reason} [{opt_out} is set; skip #{count} in this process]");
        return None;
    }
    panic!(
        "FAIL {test}: missing prerequisite: {reason}\n\
         A test that asserts nothing must not report a pass (HYG2: BACKSETTLE \
         suite3.log was green with six ART cases skipped).\n\
         Provide the prerequisite named above, or set {ALLOW_SKIP_ENV}=1 to run \
         with a counted skip (the summary then prints how many were skipped)."
    );
}

/// A prerequisite that is a value: `Some(v)` is the prerequisite, `None` is
/// missing and is reported through [`require_prereq`] (a failure by default, a
/// counted skip behind the opt-out).
///
/// Call it as `let Some(x) = require_some(expr, "test", "reason") else { return; };`
/// — the `None` path of `expr` *is* the prerequisite check, so no test
/// returns early without the failure or the counted skip having happened.
pub fn require_some<T>(value: Option<T>, test: &str, reason: &str) -> Option<T> {
    match value {
        Some(v) => Some(v),
        None => {
            require_prereq(false, test, reason);
            None
        }
    }
}

/// Print the per-process skip summary once, on normal exit.
fn install_summary() {
    SUMMARY.call_once(|| unsafe {
        // `libc::atexit` runs after the test harness finished all tests, so the
        // count is final. A panicking test still exits through here, which is
        // why the summary is printed for a failure run too.
        libc::atexit(summary_hook);
    });
}

extern "C" fn summary_hook() {
    let count = SKIPPED.load(Ordering::SeqCst);
    if count > 0 {
        eprintln!(
            "SKIP SUMMARY: {count} test(s) skipped in this process because \
             {} is set; the default path must show 0.",
            active_opt_out()
        );
    }
}

// ---------------------------------------------------------------------------
// ART prerequisites (one rule for every ART-booting test)
// ---------------------------------------------------------------------------

/// The two host artefacts an ART-booting test needs, resolved (not checked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtInputs {
    /// The ART sysroot (`system/apex` marks it as installed).
    pub sysroot: PathBuf,
    /// A warm `/data` tree with `app/guest/classes.dex`.
    pub warm_data: PathBuf,
}

/// The ART sysroot for `checkout`: `$ACOS_ART_SYSROOT`, else `<checkout>/.cache/art/sysroot`.
#[must_use]
pub fn art_sysroot(checkout: &Path) -> PathBuf {
    std::env::var_os(SYSROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| checkout.join(".cache/art/sysroot"))
}

/// The warm ART `/data` for `checkout`.
///
/// In order: `$ACOS_ART_TEST_DATA` (a hand-seeded dir), `$ACOS_ART_WARM_DATA`,
/// `$ACOS_ART_DATA` — each only when it holds a guest dex — then the checkout's
/// productised `.cache/art/data`. A variable that points at an empty scratch
/// directory is *not* an error here: it is what `run-exclusive.sh` used to hand
/// out, and the fall-through is what makes the documented command work.
#[must_use]
pub fn art_warm_data(checkout: &Path) -> PathBuf {
    let has_dex = |dir: &Path| dir.join("app/guest/classes.dex").is_file();
    for name in ["ACOS_ART_TEST_DATA", WARM_DATA_ENV, "ACOS_ART_DATA"] {
        if let Some(dir) = std::env::var_os(name).map(PathBuf::from) {
            if has_dex(&dir) {
                return dir;
            }
        }
    }
    checkout.join(".cache/art/data")
}

/// The ART inputs for `checkout`, resolved.
#[must_use]
pub fn art_inputs(checkout: &Path) -> ArtInputs {
    ArtInputs {
        sysroot: art_sysroot(checkout),
        warm_data: art_warm_data(checkout),
    }
}

/// [`art_inputs`], but a missing input is a loud failure (see [`require_prereq`]).
///
/// `test` names the test or suite in the message. The reasons name the exact
/// command that produces the missing artefact, because "SKIP" with no fix was
/// how the class survived.
pub fn require_art(checkout: &Path, test: &str) -> Option<ArtInputs> {
    let inputs = art_inputs(checkout);
    if !inputs.sysroot.join("system/apex").is_dir() {
        let reason = format!(
            "no ART sysroot at {} — run tools/art-sysroot.sh",
            inputs.sysroot.display()
        );
        require_prereq(false, test, &reason)?;
    }
    if !inputs.warm_data.join("app/guest/classes.dex").is_file() {
        let reason = format!(
            "no cached guest dex under {} — run \
             `acos-art render-app --apk fixtures/build/apps/counter.apk` once, or \
             run the tests through tools/art-runtime/test-crates.sh (it exports \
             {WARM_DATA_ENV} inside the lock)",
            inputs.warm_data.display()
        );
        require_prereq(false, test, &reason)?;
    }
    Some(inputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests below read and write process-global state (the opt-out
    /// variables and the skip counter), and `cargo test` runs them on parallel
    /// threads. Without this lock one test's `set_var` flips another's
    /// prerequisite from "fails" to "skips" and the counter deltas double-count
    /// (observed: `a_missing_value_prerequisite_fails_loudly` saw 2 skips for 1).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The opt-out is an explicit, spelled-out value; anything else (including
    /// an empty string) is not an opt-out.
    #[test]
    fn the_opt_out_is_read_as_a_boolean() {
        let _env = env_lock();
        let saved = std::env::var_os(ALLOW_SKIP_ENV);
        for (value, expected) in [
            (None, false),
            (Some(""), false),
            (Some("0"), false),
            (Some("1"), true),
            (Some("true"), true),
        ] {
            match value {
                None => std::env::remove_var(ALLOW_SKIP_ENV),
                Some(value) => std::env::set_var(ALLOW_SKIP_ENV, value),
            }
            assert_eq!(skip_allowed(), expected, "value {value:?}");
        }
        match saved {
            Some(value) => std::env::set_var(ALLOW_SKIP_ENV, value),
            None => std::env::remove_var(ALLOW_SKIP_ENV),
        }
    }

    /// HYG2's acceptance: a missing prerequisite fails **loudly** by default.
    /// This is the test that fails before the change (the old helper returned
    /// `None` and the test passed with nothing asserted).
    #[test]
    fn a_missing_prerequisite_fails_loudly_unless_the_opt_out_is_set() {
        let _env = env_lock();
        let saved = std::env::var_os(ALLOW_SKIP_ENV);
        std::env::remove_var(ALLOW_SKIP_ENV);
        std::env::remove_var(ALLOW_ANY_SKIP_ENV);
        let outcome = std::panic::catch_unwind(|| {
            require_prereq(false, "hyg2-probe", "no such artefact anywhere");
        });
        let message = outcome
            .err()
            .and_then(|payload| {
                payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
            })
            .unwrap_or_default();
        assert!(
            message.starts_with("FAIL hyg2-probe: missing prerequisite: no such artefact anywhere"),
            "the failure must name the test and the reason, got: {message:?}"
        );
        assert!(
            message.contains(ALLOW_SKIP_ENV),
            "the failure must name the opt-out, got: {message:?}"
        );

        // With the explicit opt-out the same missing prerequisite is a counted
        // skip, and the count is observable.
        let before = skipped_count();
        std::env::set_var(ALLOW_SKIP_ENV, "1");
        let skipped = require_prereq(false, "hyg2-probe", "no such artefact anywhere");
        assert!(skipped.is_none(), "the opt-out skips instead of failing");
        assert_eq!(skipped_count(), before + 1, "the skip is counted");
        std::env::remove_var(ALLOW_SKIP_ENV);

        match saved {
            Some(value) => std::env::set_var(ALLOW_SKIP_ENV, value),
            None => std::env::remove_var(ALLOW_SKIP_ENV),
        }
    }

    /// `require_some` is the same rule for a value: a missing value fails by
    /// default, and a present one is returned untouched.
    #[test]
    fn a_missing_value_prerequisite_fails_loudly() {
        let _env = env_lock();
        let saved = std::env::var_os(ALLOW_SKIP_ENV);
        let saved_any = std::env::var_os(ALLOW_ANY_SKIP_ENV);
        std::env::remove_var(ALLOW_SKIP_ENV);
        std::env::remove_var(ALLOW_ANY_SKIP_ENV);
        assert_eq!(require_some(Some(7), "hyg3-probe", "never printed"), Some(7));
        let outcome = std::panic::catch_unwind(|| {
            let _: Option<u8> = require_some(None, "hyg3-probe", "javac is absent");
        });
        assert!(outcome.is_err(), "a missing value must fail, not return None");
        std::env::set_var(ALLOW_SKIP_ENV, "1");
        let before = skipped_count();
        assert_eq!(require_some::<u8>(None, "hyg3-probe", "javac is absent"), None);
        assert_eq!(skipped_count(), before + 1, "the opt-out skip is counted");
        std::env::remove_var(ALLOW_SKIP_ENV);
        for (name, value) in [(ALLOW_SKIP_ENV, saved), (ALLOW_ANY_SKIP_ENV, saved_any)] {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }

    /// A present prerequisite is not a skip: the count stays where it was and
    /// the caller gets `Some`.
    #[test]
    fn a_present_prerequisite_is_not_a_skip() {
        let _env = env_lock();
        let before = skipped_count();
        assert!(require_prereq(true, "hyg2-probe", "never printed").is_some());
        assert_eq!(skipped_count(), before);
    }

    /// The warm data resolves to a variable that *holds a guest dex*, never to
    /// the empty scratch dir `run-exclusive.sh` hands out.
    #[test]
    fn warm_data_prefers_a_variable_that_holds_a_guest_dex() {
        let _env = env_lock();
        let dir = std::env::temp_dir().join(format!("hyg2-warm-{}", std::process::id()));
        let warm = dir.join("warm");
        let scratch = dir.join("scratch");
        std::fs::create_dir_all(warm.join("app/guest")).expect("warm dir");
        std::fs::write(warm.join("app/guest/classes.dex"), b"dex\n035").expect("dex");
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        let saved_warm = std::env::var_os(WARM_DATA_ENV);
        let saved_data = std::env::var_os("ACOS_ART_DATA");
        std::env::set_var(WARM_DATA_ENV, &scratch);
        std::env::set_var("ACOS_ART_DATA", &warm);
        assert_eq!(
            art_warm_data(Path::new("/nonexistent/checkout")),
            warm,
            "an empty ACOS_ART_WARM_DATA must fall through to a warm ACOS_ART_DATA"
        );
        std::env::set_var(WARM_DATA_ENV, &warm);
        assert_eq!(art_warm_data(Path::new("/nonexistent/checkout")), warm);
        std::env::remove_var(WARM_DATA_ENV);
        std::env::remove_var("ACOS_ART_DATA");
        assert_eq!(
            art_warm_data(Path::new("/checkout")),
            Path::new("/checkout/.cache/art/data"),
            "with no variable the checkout's productised cache is the answer"
        );
        match saved_warm {
            Some(value) => std::env::set_var(WARM_DATA_ENV, value),
            None => std::env::remove_var(WARM_DATA_ENV),
        }
        match saved_data {
            Some(value) => std::env::set_var("ACOS_ART_DATA", value),
            None => std::env::remove_var("ACOS_ART_DATA"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
