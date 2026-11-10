//! Installed EGL/GLES provider table.
//!
//! [`super::ndk::ndk_gl`] resolves every `egl*`/`gl*` import to a **structured
//! trap** that reports `EGL_NOT_INITIALIZED`/`GL_INVALID_OPERATION`. That is the
//! correct fail-closed behaviour when no GL substrate is in the process.
//!
//! When the real software GLES substrate is available the embedding runtime
//! (`acos-jni`, on the `--native` path only) installs its entry-point table
//! here; [`super::resolve`] then prefers the real implementation and the traps
//! are never reached. A provider is never installed implicitly: without an
//! explicit install the loader keeps failing closed, and a process that never
//! enables native loading never gains a guest-visible GL surface.

use std::sync::{OnceLock, RwLock};

/// `name` → entry-point address, as installed by the runtime.
pub type GlesTable = [(&'static str, usize)];

fn slot() -> &'static RwLock<Option<&'static GlesTable>> {
    static SLOT: OnceLock<RwLock<Option<&'static GlesTable>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Install the EGL/GLES entry-point table (`name` → function address).
///
/// Idempotent for the same table; a later call replaces the previous provider
/// (tests use that to install/remove a fixture).
pub fn install_table(table: &'static GlesTable) {
    if let Ok(mut g) = slot().write() {
        *g = Some(table);
    }
}

/// Remove the provider, restoring the fail-closed traps (test teardown).
pub fn clear_table() {
    if let Ok(mut g) = slot().write() {
        *g = None;
    }
}

/// Whether a provider is installed.
#[must_use]
pub fn installed() -> bool {
    slot().read().map(|g| g.is_some()).unwrap_or(false)
}

/// Look up an installed provider entry as a [`super::Shim`], keeping the
/// table's `'static` symbol name.
#[must_use]
pub fn shim(name: &str) -> Option<super::Shim> {
    let g = slot().read().ok()?;
    let table = (*g)?;
    table
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(n, p)| super::Shim::shimmed(n, *p as *const std::os::raw::c_void))
}

/// Look up an installed provider entry by symbol name.
#[must_use]
pub fn entry(name: &str) -> Option<usize> {
    let g = slot().read().ok()?;
    let table = (*g)?;
    table.iter().find(|(n, _)| *n == name).map(|(_, p)| *p)
}

#[cfg(test)]
mod tests {
    use super::*;

    static FIXTURE: &[(&str, usize)] = &[("eglGetDisplay", 0x1234)];

    #[test]
    fn an_installed_table_is_preferred_and_removable() {
        clear_table();
        assert!(entry("eglGetDisplay").is_none());
        assert!(!installed());
        install_table(FIXTURE);
        assert!(installed());
        assert_eq!(entry("eglGetDisplay"), Some(0x1234));
        assert_eq!(entry("glClear"), None);
        clear_table();
        assert!(entry("eglGetDisplay").is_none());
    }
}
