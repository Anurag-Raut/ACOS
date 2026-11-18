//! Deferred Landlock filesystem confinement for the ART guest (S6).
//!
//! `results/art-runtime/S1-sandbox-review.md` §5 #6 asked for a Landlock
//! ruleset applied to the guest *after* bwrap's mounts. S5 deferred it because
//! there was no in-guest hook and no exported policy. S6 closes both:
//!
//! * the policy is assembled by **`acos-sandbox`** (`landlock_guest_plan`), so
//!   there is still exactly one Landlock policy in the tree (ADR-0022);
//! * the plan is written next to the seccomp guard artefacts in
//!   `<data>/seccomp/`, which every filtered run already binds read-only at
//!   `/data/seccomp`;
//! * the freestanding preload constructor in [`crate::seccomp`] applies it with
//!   the raw syscalls (`landlock_create_ruleset` → `landlock_add_rule` →
//!   `landlock_restrict_self`) *before* it installs the seccomp filter, i.e.
//!   after bionic has loaded and before ART's `main` or any JNI code runs.
//!
//! ## What the ruleset says
//!
//! * `handled_access_fs` is the full ABI mask from `acos-sandbox`, so every
//!   write-class right (`WRITE_FILE`, `REMOVE_*`, `MAKE_*`, `REFER`,
//!   `TRUNCATE`, `IOCTL_DEV`) is handled — a right that is handled and not
//!   granted is **denied**.
//! * The only read-only root is `/` (the guest's own mount view): ART needs to
//!   read `/system`, `/apex`, `/data/system`, `/data/app`, `/data/img`,
//!   `/linkerconfig`, `/proc`, `/sys` and `/dev` and there is nothing host-side
//!   under it — bwrap already made that view minimal and read-only where it
//!   matters.
//! * Writes are granted **only** on the per-run root's writable binds plus the
//!   device nodes a process legitimately writes (`/dev/null`, `/dev/zero`,
//!   `/dev/*random`, `/dev/full`, `/dev/tty`) and `/dev/socket` (logd). Writing
//!   anywhere else in the guest — `/data` itself, `/data/system`,
//!   `/data/local/tmp`, `/linkerconfig`, `/system`, `/` — now returns `EACCES`
//!   even though DAC would have allowed it.
//!
//! ## Fail closed, with one documented fallback
//!
//! The kernel Landlock ABI is probed on the **host** (`acos_sandbox::landlock_abi`)
//! and again inside the guest by the applier:
//!
//! * ABI present ⇒ the plan is mandatory. A missing/unreadable plan, a failed
//!   `create_ruleset`/`add_rule`/`restrict_self`, or a constructor that exits
//!   before logging `OK landlock=active` **fails the run**
//!   (`Error::LandlockUnavailable`, stable code `LANDLOCK_UNAVAILABLE`). The
//!   layer ships in the same guard bundle as the seccomp filter, but it is a
//!   distinct failure: filesystem confinement, not syscall filtering.
//! * ABI absent (kernel < 5.13 or Landlock not in the LSM list) ⇒ no plan is
//!   written; the applier logs `SKIP landlock=kernel-unsupported` and the run
//!   proceeds on the S2–S4 namespace/mount view, whose write surface is already
//!   per-run. This is the documented fallback: the report says the layer is
//!   inactive rather than pretending it was applied.
//!
//! Denials are `EACCES`, like the seccomp filter's: Landlock has no audit
//! channel without `CAP_SYS_ADMIN` in the initial user namespace. The applied
//! ruleset (paths and masks) is in the run's evidence file, and the audit
//! procedure in `results/art-runtime/S6.md` establishes that a normal run never
//! needs a denied path.
//!
//! Scopes (ABI ≥ 6 signal/abstract-socket confinement) are deliberately *not*
//! enabled for the guest: bionic installs its own signal handlers after
//! constructors, and the guest pid namespace plus the seccomp `kill`/`tkill`
//! denial already bound signalling. Recorded as a residual in S6.md.

use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// File name of the plan inside `<data>/seccomp` (also `/data/seccomp` in the
/// guest, which every filtered run already binds read-only).
pub const PLAN_FILE: &str = "landlock.plan";

/// Guest path the applier reads.
pub const GUEST_PLAN: &str = "/data/seccomp/landlock.plan";

/// Read roots: the guest's whole mount view. ART's boot classpath, APEX
/// payloads, boot image, linkerconfig, `/proc`, `/sys` and `/dev` are all read
/// from here; bwrap has already removed everything host-side that is not part
/// of this view.
pub const READ_ONLY_ROOTS: &[&str] = &["/"];

/// The only writable roots: the per-run `/data` binds (S4) and the device
/// nodes a process writes.
pub const READ_WRITE_ROOTS: &[&str] = &[
    "/data/dalvik-cache",
    "/data/misc",
    "/data/tmp",
    "/data/out",
    "/data/task",
    "/data/app-data",
    "/data/session",
    "/dev/socket",
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
];

/// Write (or, on a kernel without Landlock, remove) the guest plan in
/// `dir` (the `<data>/seccomp` directory).
///
/// Returns `Ok(false)` when Landlock is absent on this kernel: the caller
/// records the documented fallback. Any other failure is an error — a kernel
/// that supports Landlock must get the layer.
pub fn write_plan(dir: &Path) -> Result<bool> {
    let plan_path = dir.join(PLAN_FILE);
    if acos_sandbox::landlock_abi().is_none() {
        // Kernel without Landlock: remove a stale plan so a run never claims a
        // layer the kernel cannot enforce (the applier also checks itself).
        match std::fs::remove_file(&plan_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(Error::Io(e));
            }
        }
        return Ok(false);
    }
    let bytes =
        acos_sandbox::landlock_guest_plan(READ_ONLY_ROOTS, READ_WRITE_ROOTS).map_err(|e| {
            Error::LandlockUnavailable(format!("cannot assemble the guest ruleset: {e}"))
        })?;
    // H17: this file is bind-mounted read-only into every guest of the cohort and
    // read by the guard constructor at start. Rewriting it in place could hand a
    // starting guest an empty file (`ERR landlock=plan-missing`, exit 126), so it
    // is published atomically.
    crate::publish_atomic(&plan_path, &bytes)?;
    Ok(true)
}

/// GPUSPIKE: the plan for a run that opted into a host-GPU provider, written
/// next to the default plan as `landlock.gpu.<node>.plan` and bound over
/// `/data/seccomp/landlock.plan` for **that run only**.
///
/// It is the default plan plus exactly one extra read-write root, the DRM
/// render node (`/dev/dri/renderD<N>`): `WRITE_FILE` + `IOCTL_DEV` are what
/// `open(O_RDWR)` and every `DRM_IOCTL_*` need, and without them the guest's
/// Mesa logs `failed to open /dev/dri/renderD129: Permission denied` and falls
/// back to llvmpipe (measured, `results/handoff/GPUSPIKE/p1`). The default
/// plan, and therefore every run without the flag, is unchanged.
///
/// `Ok(None)` when this kernel has no Landlock (nothing to widen).
pub fn write_gpu_plan(dir: &Path, render_node: &Path) -> Result<Option<PathBuf>> {
    if acos_sandbox::landlock_abi().is_none() {
        return Ok(None);
    }
    let node = render_node.to_string_lossy().to_string();
    if !crate::gles::valid_render_node(&node) {
        return Err(Error::Layout(format!(
            "{node:?} is not /dev/dri/renderD<N>; refusing to widen the guest's Landlock plan"
        )));
    }
    let mut rw: Vec<&str> = READ_WRITE_ROOTS.to_vec();
    rw.push(&node);
    let bytes = acos_sandbox::landlock_guest_plan(READ_ONLY_ROOTS, &rw)
        .map_err(|e| Error::LandlockUnavailable(format!("cannot assemble the GPU ruleset: {e}")))?;
    let base = node.rsplit('/').next().unwrap_or("renderD");
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("landlock.gpu.{base}.plan"));
    crate::publish_atomic(&path, &bytes)?;
    Ok(Some(path))
}

/// Require the applier's evidence in a finished run's guard log.
///
/// `OK landlock=active abi=N rules=M` means the kernel accepted the ruleset;
/// `SKIP landlock=kernel-unsupported` is the only accepted absence (the
/// documented fallback for a kernel without Landlock). Anything else fails the
/// run, because on a kernel that supports Landlock the layer is mandatory.
pub fn require_from_log(text: &str, log_path: &Path) -> Result<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("OK landlock=active") || line.starts_with("SKIP landlock=") {
            return Ok(line.to_string());
        }
    }
    Err(Error::LandlockUnavailable(format!(
        "the guest Landlock ruleset is not proven in {}; \
         a kernel with Landlock must apply it or the run is refused",
        log_path.display()
    )))
}

/// The plan text, for diagnostics/tests (empty when Landlock is unsupported).
pub fn plan_preview() -> Result<String> {
    if acos_sandbox::landlock_abi().is_none() {
        return Ok(String::new());
    }
    let bytes = acos_sandbox::landlock_guest_plan(READ_ONLY_ROOTS, READ_WRITE_ROOTS)
        .map_err(|e| Error::LandlockUnavailable(e.to_string()))?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gpu_plan_adds_exactly_the_render_node_and_nothing_else() {
        if acos_sandbox::landlock_abi().is_none() {
            eprintln!("SKIP landlock gpu plan test: this kernel has no Landlock");
            return;
        }
        let dir = std::env::temp_dir().join(format!("acos-gpu-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = write_gpu_plan(&dir, Path::new("/dev/dri/renderD129"))
            .expect("plan")
            .expect("landlock present");
        assert!(path.ends_with("landlock.gpu.renderD129.plan"), "{path:?}");
        let gpu = std::fs::read_to_string(&path).expect("read");
        let base = plan_preview().expect("default plan");
        let rw = |t: &str| -> Vec<String> {
            t.lines()
                .filter_map(|l| l.strip_prefix("rw "))
                .map(|l| l.splitn(2, ' ').nth(1).unwrap_or("").to_string())
                .collect()
        };
        let (g, b) = (rw(&gpu), rw(&base));
        assert!(g.contains(&"/dev/dri/renderD129".to_string()), "{g:?}");
        assert_eq!(g.len(), b.len() + 1, "exactly one extra root: {g:?} vs {b:?}");
        assert!(!b.contains(&"/dev/dri/renderD129".to_string()), "default plan must not grant it");
        // Never a card node (modesetting), never a directory.
        assert!(write_gpu_plan(&dir, Path::new("/dev/dri/card0")).is_err());
        assert!(write_gpu_plan(&dir, Path::new("/dev/dri")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_grants_writes_only_on_the_per_run_roots() {
        if acos_sandbox::landlock_abi().is_none() {
            eprintln!("SKIP landlock plan test: this kernel has no Landlock");
            return;
        }
        let text = plan_preview().expect("plan");
        assert!(text.starts_with("acos-landlock 1\n"), "{text}");
        assert!(text.contains("ro "), "there is a read-only root: {text}");
        let rw: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("rw ").and_then(|r| r.splitn(2, ' ').nth(1)))
            .collect();
        assert!(
            rw.contains(&"/data/out"),
            "the run's out dir is writable: {rw:?}"
        );
        assert!(rw.contains(&"/data/task"), "{rw:?}");
        // The escape targets are *not* writable roots.
        for forbidden in [
            "/",
            "/data",
            "/data/system",
            "/data/local/tmp",
            "/linkerconfig",
            "/system",
            "/proc",
        ] {
            assert!(
                !rw.contains(&forbidden),
                "{forbidden} must not be writable: {rw:?}"
            );
        }
    }

    /// E1: a Landlock failure carries `LANDLOCK_UNAVAILABLE`, not the seccomp
    /// family's `SECCOMP_UNAVAILABLE`; the two layers are distinguishable.
    #[test]
    fn landlock_failure_has_its_own_code() {
        let err = Error::LandlockUnavailable("plan missing".into());
        assert_eq!(err.to_string(), "LANDLOCK_UNAVAILABLE: plan missing");
        assert!(!err.to_string().contains("SECCOMP_UNAVAILABLE"));
    }

    #[test]
    fn fallback_is_only_accepted_as_an_explicit_skip() {
        let p = Path::new("/nonexistent/landlock.log");
        assert!(require_from_log("OK landlock=active abi=10 rules=9\n", p).is_ok());
        assert!(require_from_log("SKIP landlock=kernel-unsupported\n", p).is_ok());
        assert!(require_from_log("OK guard=active insns=82\n", p).is_err());
        assert!(require_from_log("ERR landlock=restrict errno=1\n", p).is_err());
        assert!(require_from_log("", p).is_err());
    }
}
