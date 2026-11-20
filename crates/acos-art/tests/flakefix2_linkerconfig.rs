//! FLAKEFIX2: the `/linkerconfig` tree generated for a **fresh** install (no
//! build-cache entry, e.g. a new worktree or a first launch under a new prefix)
//! must be the legacy, unisolated config the runtime is built for.
//!
//! The image's `linkerconfig` chooses its mode from `ro.vndk.version`. GATENE2E
//! added that key to the guests' property table (libcutils' memfd ashmem needs
//! it), which silently switched a *regenerated* tree to the strict Treble
//! config: `derive_classpath` then failed to open `/data/shim/libartshim.so`
//! (the default namespace no longer permits `/data/shim`) and `dalvikvm64`
//! could not find `libnativehelper.so`. Only a cache miss regenerated, so every
//! test passed on the main checkout and a fresh worktree failed.
//!
//! This test runs the image's real `linkerconfig` into an empty data dir with
//! the build cache off, so it fails without the generation-table fix.

use acos_art::ArtRoot;
use std::path::{Path, PathBuf};

fn scratch(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cache/flakefix2-tests")
        .join(format!("{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch under .cache/");
    dir
}

#[test]
fn a_regenerated_linkerconfig_is_the_unisolated_legacy_config() {
    let dir = scratch("legacy");
    std::env::set_var("ACOS_ART_BUILD_CACHE", "off");
    let root = ArtRoot::resolve(None, Some(dir.join("data")))
        .expect("the extracted sysroot (.cache/art/sysroot) must be present for this test");
    root.ensure_linkerconfig(true)
        .expect("the image's linkerconfig runs");
    let text = std::fs::read_to_string(dir.join("data/linkerconfig/ld.config.txt")).unwrap();
    // The strict (Treble-style) config has `dir.system` sections and an isolated
    // default namespace; the legacy one has `[legacy]` and
    // `namespace.default.isolated = false`, which is what lets the
    // `LD_PRELOAD`ed `/data/shim/libartshim.so` open.
    let excerpt = &text[..text.len().min(1500)];
    assert!(
        !text.contains("dir.system") && !text.contains("[system]"),
        "regeneration produced the strict `dir.system` config:\n{excerpt}"
    );
    assert!(
        text.contains("[legacy]") && text.contains("namespace.default.isolated = false"),
        "regeneration did not produce the legacy unisolated config:\n{excerpt}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
