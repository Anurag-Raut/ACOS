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
