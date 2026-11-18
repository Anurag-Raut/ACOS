//! The guest's software GL provider — `acos-art`'s thin view of
//! [`acos_gles_provider`].
//!
//! The contract itself (the `ACOS_GUEST_GLES` selection rule, the provider
//! manifest, the sha256 verify cache, the render node and the extra properties)
//! lives in the `acos-gles-provider` crate, because three front doors report or
//! apply it and they must not each carry a copy: this launcher (the bind and the
//! property table), `acos doctor` and `acos_capabilities`. What stays here is
//! only the translation from an [`ArtRoot`] to the sysroot path that crate takes.
//!
//! Nothing in it is time-critical: [`status`] is one manifest read and no bytes,
//! [`verify`] is the cached stat fingerprint, and a run with no provider pays
//! neither.

use crate::{ArtRoot, Result};
use std::path::Path;

pub use acos_gles_provider::{
    capability_for, describe, sha256_hex, staged_bytes, valid_prop_key, valid_render_node,
    GlesFile, GlesManifest, Provider, Status, DEFAULT_PROVIDER, DIR_ENV, ENV, GUEST_EGL_DIR,
    MANIFEST, OFF, RENDER_NODE_ENV, RESERVED_MESA_HOST, VERIFIED,
};

/// Does the provider's bind source exist and hold the EGL directory?
#[must_use]
pub fn egl_dir_ready(provider: &Provider, root: &ArtRoot) -> bool {
    acos_gles_provider::egl_dir_ready(provider, Path::new(&root.sysroot))
}

/// Does the bound directory (the guest's `/vendor/lib64`) exist?
#[must_use]
pub fn lib_dir_ready(provider: &Provider, root: &ArtRoot) -> bool {
    acos_gles_provider::lib_dir_ready(provider, Path::new(&root.sysroot))
}

/// `<sysroot>/gles` (or the `ACOS_GUEST_GLES_DIR` override base).
#[must_use]
pub fn providers_root(root: &ArtRoot) -> std::path::PathBuf {
    acos_gles_provider::providers_root(Path::new(&root.sysroot))
}

/// `<path>` relative to a provider dir, for the install tool's manifest.
#[must_use]
pub fn relative(path: &Path, base: &Path) -> String {
    acos_gles_provider::relative(path, base)
}

/// Resolve the provider for this run: the flag, else the installed default.
///
/// The one selection rule — the bind, the property table and `acos doctor` all
/// call it, so "installed" cannot mean two things.
pub fn status(root: &ArtRoot) -> Result<Status> {
    Ok(acos_gles_provider::status(&root.sysroot)?)
}

/// The provider to bind, if any (see [`status`]).
pub fn selected(root: &ArtRoot) -> Result<Option<Provider>> {
    Ok(acos_gles_provider::selected(&root.sysroot)?)
}

/// Load and parse `<sysroot>/gles/<name>/manifest.json` (no hashing; [`verify`]
/// checks the bytes).
pub fn load(root: &ArtRoot, name: &str) -> Result<Provider> {
    Ok(acos_gles_provider::load(&root.sysroot, name)?)
}

/// Verify the staged files, hashing them only when the stat fingerprint moved.
pub fn verify(provider: &Provider, root: &ArtRoot) -> Result<()> {
    Ok(acos_gles_provider::verify(provider, &root.sysroot)?)
}

/// Verify every staged file against the manifest's sha256 and size, always.
pub fn verify_full(provider: &Provider, root: &ArtRoot) -> Result<()> {
    Ok(acos_gles_provider::verify_full(provider, &root.sysroot)?)
}

/// `<provider>/.verified` (see [`VERIFIED`]).
#[must_use]
pub fn verify_cache_path(provider: &Provider, root: &ArtRoot) -> std::path::PathBuf {
    acos_gles_provider::verify_cache_path(provider, &root.sysroot)
}

/// The `(ro.hardware.egl, ro.hardware.vulkan)` values the **selected**
/// provider declares, or `None` when software GL is off.
///
/// `Props::for_launch()` runs in places that do not hold an [`ArtRoot`] (main's
/// `props print`, the session/app writers), so this resolves the sysroot the
/// same way [`ArtRoot::resolve`] does. A missing sysroot is not an error here: a
/// run with a provider selected fails loudly in `Sandbox::command` (the bind
/// source check) before any guest starts, and this is only the property table.
pub fn selected_tags() -> Result<Option<(String, Option<String>)>> {
    Ok(selected_props()?.map(|(egl, vulkan, _)| (egl, vulkan)))
}

/// [`selected_tags`] plus the provider's extra properties (GPUSPIKE: Waydroid's
/// `ro.hardware.gralloc`/`gralloc.gbm.device`), in the same one selection rule.
pub fn selected_props() -> Result<Option<(String, Option<String>, Vec<(String, String)>)>> {
    let root = match ArtRoot::resolve(None, None) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    Ok(acos_gles_provider::selected_props_for(&root.sysroot)?)
}

/// The `gles` capability line for `acos doctor` / `acos_capabilities`:
/// `angle-swiftshader` when a provider is selected (the default one, or the
/// `ACOS_GUEST_GLES` override), else `none`, plus the explanation.
///
/// Never an error: a host without the sysroot, without the provider or with an
/// unreadable manifest reports `none` and why, because this is a *report*, and
/// the launch path is where a broken explicit selection must fail loudly.
#[must_use]
pub fn capability() -> (String, String) {
    match ArtRoot::resolve(None, None) {
        Ok(root) => acos_gles_provider::capability_for(Some(&root.sysroot)),
        Err(e) => ("none".to_string(), e.to_string()),
    }
}

/// `ACOS_GUEST_GLES`'s value as the sysroot-relative provider name, if any.
///
/// Kept on this side because the launcher never needs it: it exists for the
/// install tooling and for the doctor's diagnostics.
pub fn provider_name_from_env() -> Result<Option<String>> {
    Ok(acos_gles_provider::provider_name_from_env()?)
}

/// `<provider>/lib64` for an [`ArtRoot`] (see [`Provider::lib_dir`]).
#[must_use]
pub fn lib_dir(provider: &Provider, root: &ArtRoot) -> std::path::PathBuf {
    provider.lib_dir(Path::new(&root.sysroot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ArtRoot;

    /// A scratch root with a staged provider under `<dir>/sysroot/gles/<name>`.
    fn root_with_provider(tag: &str) -> (ArtRoot, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "acos-art-gles-facade-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let root = ArtRoot {
            repo_root: dir.clone(),
            sysroot: dir.join("sysroot"),
            data: dir.join("data"),
            modules: Vec::new(),
        };
        let egl = root.sysroot.join("gles/angle-swiftshader/lib64/egl");
        std::fs::create_dir_all(&egl).expect("dirs");
        std::fs::write(egl.join("libEGL_angle.so"), b"driver bytes").expect("driver");
        let manifest = GlesManifest {
            tag: "angle".into(),
            vulkan_tag: None,
            props: std::collections::BTreeMap::new(),
            render_node: None,
            provider: "test".into(),
            source: "unit test".into(),
            version: "0".into(),
            licence: "Apache-2.0".into(),
            files: vec![GlesFile {
                path: "lib64/egl/libEGL_angle.so".into(),
                sha256: sha256_hex(b"driver bytes"),
                size: 12,
            }],
        };
        std::fs::write(
            root.sysroot.join("gles/angle-swiftshader/manifest.json"),
            serde_json::to_string_pretty(&manifest).expect("json"),
        )
        .expect("manifest");
        (root, dir)
    }

    /// MERGE040D(a): the facade must resolve `<ArtRoot as sysroot>/gles`, so the
    /// launcher binds exactly what the shared contract verified. A wrong
    /// translation would look for the provider under a different directory and
    /// silently report "not installed" for a staged one — this test fails
    /// before the split (there was no facade) and pins the seam after it.
    #[test]
    fn the_facade_resolves_the_sysroot_the_provider_crate_takes() {
        let (root, dir) = root_with_provider("status");
        let status = status(&root).expect("status");
        let provider = status.provider.as_ref().expect("the staged default");
        assert_eq!(provider.name, DEFAULT_PROVIDER);
        assert!(status.default, "the default provider is installed");
        assert_eq!(lib_dir(provider, &root), root.sysroot.join("gles/angle-swiftshader/lib64"));
        assert!(lib_dir_ready(provider, &root));
        assert!(egl_dir_ready(provider, &root));
        // The verify cache travels with the provider directory (one rule).
        verify(provider, &root).expect("verified");
        assert!(verify_cache_path(provider, &root).is_file());
        assert_eq!(
            verify_cache_path(provider, &root),
            provider.dir(&root.sysroot).join(VERIFIED)
        );
        // ... and the property view the launcher serves comes from the same one.
        let (tag, vulkan, extra) = acos_gles_provider::selected_props_for(&root.sysroot)
            .expect("props")
            .expect("the staged default");
        assert_eq!(tag, "angle");
        assert_eq!(vulkan, None);
        assert!(extra.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
