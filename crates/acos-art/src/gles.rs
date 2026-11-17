//! GLSPIKE: an **opt-in, lazily loaded** software GLES provider for the guest.
//!
//! The image ships the framework's EGL *loader* (`/system/lib64/libEGL.so`) but
//! no driver: `ro.hardware.egl`/`persist.graphics.egl`/`ro.board.platform` are
//! unset and there is no `/system/lib64/egl` or `/vendor/lib64/egl` in the
//! sysroot, so `Loader::open()` reaches its
//! `LOG_ALWAYS_FATAL_IF(!hnd, "couldn't find an OpenGL ES implementation, ...")`
//! and the process aborts the first time libhwui wants a GL context (a
//! HARDWARE `Bitmap` draw, a `TextureView`, Glide/Coil's default hardware
//! bitmaps). PLT interposition of `eglGetDisplay` did not help (A11YPACE §4):
//! the loader is the component that dies, not its caller.
//!
//! The loader's *own* search path is the seam this module uses
//! (`frameworks/native/opengl/libs/EGL/Loader.cpp`, android-14.0.0_r75, measured
//! against the sysroot's `libEGL.so`: the strings `libEGL_angle failed!`,
//! `_angle.so` and `/system/lib64/egl` are in it):
//!
//! * `attempt_to_load_system_driver(cnx, prop, true)` looks for
//!   `libEGL_<prop>.so`, `libGLESv1_CM_<prop>.so`, `libGLESv2_<prop>.so` in
//!   `/vendor/lib64/egl` and `/system/lib64/egl`;
//! * `prop` is `persist.graphics.egl`, else `ro.hardware.egl`, else
//!   `ro.board.platform` (`HAL_SUBNAME_KEY_PROPERTIES`).
//!
//! So a provider is: three real, bionic-built `.so` files in the guest's
//! `/vendor/lib64/egl` plus `ro.hardware.egl=<tag>`. Nothing else changes.
//!
//! ## Laziness and cost
//!
//! [`Sandbox::command`] bind-mounts the provider's directory **read-only** and
//! the property is a string in the per-run table. Neither maps a page of the
//! driver: `libEGL.so` is dlopened by libhwui when it first needs GL (the
//! RenderThread's EGL context), and only then does `Loader::open()` dlopen the
//! driver. A process that never touches EGL pays **zero** resident bytes (the
//! bind mount costs a mount entry, not memory) — measured in
//! `results/handoff/GLSPIKE/README.md` §5. When the flag is off this module is
//! never asked for anything: no bind, no property, no digest.
//!
//! ## Default OFF, and it says why when it cannot
//!
//! `ACOS_GUEST_GLES=<provider>` selects `<data>/gles/<provider>/`. The provider
//! directory carries a `manifest.json` (source URL, version, licence, sha256 per
//! file) written by `tools/art-gles.sh`; the launcher verifies every file's
//! sha256 against it on **every** launch (the A11YPACE stale-shim lesson: a
//! shared prebuilt that silently predates its contract must fail loudly). A
//! missing or mismatched provider is a structured [`Error::Layout`], never a
//! silent skip that would reach libEGL's fatal assert instead.
//!
//! Licence/source are part of the manifest, not of this module: the spike ships
//! no GL code, only the mechanism to stage someone else's.

use crate::{ArtRoot, Error, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The environment variable that turns the provider on. Default (absent or
/// empty) is off.
pub const ENV: &str = "ACOS_GUEST_GLES";

/// The guest directory the loader searches for a suffixed driver.
pub const GUEST_EGL_DIR: &str = "/vendor/lib64/egl";

/// The provider's manifest file name inside `<sysroot>/gles/<provider>/`.
pub const MANIFEST: &str = "manifest.json";

/// Where a provider may be installed outside the sysroot (spike/private arms).
///
/// Absolute host directory; it must contain `manifest.json` and `lib64/egl/`
/// exactly like `<sysroot>/gles/<tag>`. It exists so an arm can be staged
/// without writing into the shared sysroot; the product location is the sysroot
/// (`<sysroot>/gles/<tag>`, the same add-on path as `<sysroot>/shim/`).
pub const DIR_ENV: &str = "ACOS_GUEST_GLES_DIR";

/// One staged driver file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlesFile {
    /// Path relative to the provider directory (e.g. `lib64/egl/libEGL_angle.so`).
    pub path: String,
    /// sha256 of the file's bytes, lowercase hex.
    pub sha256: String,
    pub size: u64,
}

/// What `tools/art-gles.sh` writes and the launcher verifies.
///
/// `tag` is the `ro.hardware.egl` value the loader appends (`libEGL_<tag>.so`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlesManifest {
    /// `ro.hardware.egl` value, e.g. `angle`.
    pub tag: String,
    /// `ro.hardware.vulkan` value when the provider also carries a Vulkan ICD
    /// (`lib64/hw/vulkan.<vulkan_tag>.so`), e.g. `pastel` for SwiftShader.
    ///
    /// GLSPIKE2: ANGLE's Vulkan backend is only as good as the ICD the image's
    /// `libvulkan.so` can find, and `libvulkan.so` names that ICD from this
    /// property (`hwvulkan module open`, `vulkan.<tag>`, measured in the
    /// sysroot's own libvulkan). `None` = no ICD, i.e. GLSPIKE's arm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vulkan_tag: Option<String>,
    /// Human name of the provider, e.g. `ANGLE on gfxstream (emulator 34.1.9)`.
    pub provider: String,
    /// Where the bytes came from (URL or local image path), for the report.
    pub source: String,
    /// Upstream version, verbatim.
    pub version: String,
    /// SPDX licence id(s), verbatim from upstream.
    pub licence: String,
    pub files: Vec<GlesFile>,
}

/// The provider the caller selected, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    /// Directory name under `<data>/gles/`.
    pub name: String,
    pub manifest: GlesManifest,
}

impl Provider {
    /// `<sysroot>/gles/<name>`, or `$ACOS_GUEST_GLES_DIR` when set.
    ///
    /// The sysroot is where the shared, read-only guest inputs live (the shim
    /// next to it, `shim/`), and it is **not** per-run: `run-exclusive.sh`
    /// hands every run a fresh `ACOS_ART_DATA`, so a provider installed under
    /// the data dir would vanish with the run. Nothing is loaded from here
    /// unless the run opted in.
    #[must_use]
    pub fn dir(&self, root: &ArtRoot) -> PathBuf {
        if let Some(dir) = provider_dir_override() {
            return dir.join(&self.name);
        }
        root.sysroot.join("gles").join(&self.name)
    }

    /// The `ro.hardware.egl` value.
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.manifest.tag
    }

    /// The `ro.hardware.vulkan` value, when the provider stages a Vulkan ICD.
    #[must_use]
    pub fn vulkan_tag(&self) -> Option<&str> {
        self.manifest.vulkan_tag.as_deref()
    }

    /// `<provider>/lib64/hw/vulkan.<vulkan_tag>.so`, the ICD the image's
    /// `libvulkan.so` opens through `hw_get_module("vulkan", tag)`. It lives in
    /// the same `lib64/` the launcher binds at `/vendor/lib64`, so no second
    /// bind is needed.
    #[must_use]
    pub fn icd_path(&self, root: &ArtRoot) -> Option<PathBuf> {
        let tag = self.vulkan_tag()?;
        Some(self.lib_dir(root).join("hw").join(format!("vulkan.{tag}.so")))
    }

    /// The host directory the guest's `/vendor/lib64` is bound to.
    ///
    /// The provider carries the drivers **twice**, as hardlinks: in `lib64/egl`
    /// (where libEGL's `findLibrary` looks for `libEGL_<tag>.so`) and in
    /// `lib64` itself (where the guest linker resolves the driver's own
    /// `DT_NEEDED`, because this image's generated `ld.config.txt` gives the
    /// default namespace `/vendor/${LIB}` but **not** `/vendor/${LIB}/egl` —
    /// no sphal namespace exists at all, since the sysroot has no vendor
    /// partition). One bind covers both searches, so a driver whose EGL lib
    /// NEEDs its GLES lib is loadable without touching linkerconfig.
    #[must_use]
    pub fn lib_dir(&self, root: &ArtRoot) -> PathBuf {
        self.dir(root).join("lib64")
    }

    /// The provider's `lib64/egl` (what libEGL's `findLibrary` searches).
    #[must_use]
    pub fn egl_dir(&self, root: &ArtRoot) -> PathBuf {
        self.lib_dir(root).join("egl")
    }
}

/// `$ACOS_GUEST_GLES_DIR` as an absolute host directory, if set.
fn provider_dir_override() -> Option<PathBuf> {
    let raw = std::env::var(DIR_ENV).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

/// Read `ACOS_GUEST_GLES` and load/verify the provider it names.
///
/// Returns `Ok(None)` when the variable is absent or empty (the default: the
/// guest keeps today's behaviour and libEGL's fatal is unchanged).
pub fn selected(root: &ArtRoot) -> Result<Option<Provider>> {
    match provider_name_from_env()? {
        Some(name) => Ok(Some(load(root, &name)?)),
        None => Ok(None),
    }
}

/// The provider name from `ACOS_GUEST_GLES`, validated as a plain directory
/// name (no separators, no `.`/`..`, so the flag can never point the bind mount
/// anywhere else). `None` when the variable is absent or empty.
///
/// The name **is** the `ro.hardware.egl` value: the loader appends the property
/// to `libEGL_`/`libGLESv1_CM_`/`libGLESv2_`, and the manifest must agree
/// ([`load`] enforces it). One spelling, so a run cannot load `libEGL_angle.so`
/// while claiming a different tag.
pub fn provider_name_from_env() -> Result<Option<String>> {
    let raw = match std::env::var(ENV) {
        Ok(v) => v,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(e) => return Err(Error::Layout(format!("{ENV}: {e}"))),
    };
    let name = raw.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(Error::Layout(format!(
            "ACOS_GUEST_GLES={name:?} is not a provider directory name"
        )));
    }
    Ok(Some(name.to_string()))
}

/// The `ro.hardware.egl` value for this run, or `None` when the flag is off.
pub fn tag_from_env() -> Result<Option<String>> {
    provider_name_from_env()
}

/// Load and verify `<data>/gles/<name>/manifest.json`.
pub fn load(root: &ArtRoot, name: &str) -> Result<Provider> {
    let dir = match provider_dir_override() {
        Some(base) => base.join(name),
        None => providers_root(root).join(name),
    };
    let manifest_path = dir.join(MANIFEST);
    let text = fs::read_to_string(&manifest_path).map_err(|e| {
        Error::Layout(format!(
            "ACOS_GUEST_GLES={name}: {} is unreadable ({e}); install the provider with \
             `tools/art-gles.sh`",
            manifest_path.display()
        ))
    })?;
    let manifest: GlesManifest = serde_json::from_str(&text).map_err(|e| {
        Error::Layout(format!(
            "ACOS_GUEST_GLES={name}: {} is not a provider manifest ({e})",
            manifest_path.display()
        ))
    })?;
    if manifest.tag.is_empty() || manifest.tag.contains('/') {
        return Err(Error::Layout(format!(
            "ACOS_GUEST_GLES={name}: manifest tag {:?} is not a ro.hardware.egl value",
            manifest.tag
        )));
    }
    if let Some(vt) = manifest.vulkan_tag.as_deref() {
        if vt.is_empty() || vt.contains('/') || vt.contains('.') {
            return Err(Error::Layout(format!(
                "ACOS_GUEST_GLES={name}: manifest vulkan_tag {vt:?} is not a ro.hardware.vulkan value"
            )));
        }
    }
    if manifest.files.is_empty() {
        return Err(Error::Layout(format!(
            "ACOS_GUEST_GLES={name}: manifest lists no files"
        )));
    }
    // The provider directory name is a label (`angle-swiftshader`), the tag is
    // the property the loader appends. What must agree is the tag and the
    // staged file names, or the loader would open a file that is not the one
    // the report describes. Checked here, and again over every file by
    // [`verify`].
    let egl_name = format!("lib64/egl/libEGL_{}.so", manifest.tag);
    if !manifest.files.iter().any(|f| f.path == egl_name) {
        return Err(Error::Layout(format!(
            "ACOS_GUEST_GLES={name}: manifest tag is {:?} but it stages no {egl_name}",
            manifest.tag
        )));
    }
    if let Some(vt) = manifest.vulkan_tag.as_deref() {
        let icd = format!("lib64/hw/vulkan.{vt}.so");
        if !manifest.files.iter().any(|f| f.path == icd) {
            return Err(Error::Layout(format!(
                "ACOS_GUEST_GLES={name}: manifest vulkan_tag is {vt:?} but it stages no {icd}"
            )));
        }
    }
    Ok(Provider {
        name: name.to_string(),
        manifest,
    })
}

/// Verify every staged file against the manifest's sha256 and size.
///
/// Called once per launch with the flag on (measured: see the report's launch
/// cost table). A mismatch is a hard error: the whole point of the flag is that
/// a *known* driver is loaded, so a shared prebuilt that drifted cannot reach
/// libEGL as a different library than the report describes.
pub fn verify(provider: &Provider, root: &ArtRoot) -> Result<()> {
    let dir = provider.dir(root);
    for f in &provider.manifest.files {
        let path = dir.join(&f.path);
        let meta = fs::metadata(&path).map_err(|e| {
            Error::Layout(format!(
                "ACOS_GUEST_GLES={}: {} is missing ({e})",
                provider.name,
                path.display()
            ))
        })?;
        if meta.len() != f.size {
            return Err(Error::Layout(format!(
                "ACOS_GUEST_GLES={}: {} is {} bytes, manifest says {}",
                provider.name,
                path.display(),
                meta.len(),
                f.size
            )));
        }
        let bytes = fs::read(&path).map_err(|e| {
            Error::Layout(format!(
                "ACOS_GUEST_GLES={}: {} is unreadable ({e})",
                provider.name,
                path.display()
            ))
        })?;
        let got = crate::build_cache::sha256_hex(&bytes);
        if got != f.sha256 {
            return Err(Error::Layout(format!(
                "ACOS_GUEST_GLES={}: {} sha256 {got} != manifest {} (reinstall with \
                 `tools/art-gles.sh`)",
                provider.name,
                path.display(),
                f.sha256
            )));
        }
    }
    Ok(())
}

/// Does the provider's bind source exist and hold the EGL directory?
#[must_use]
pub fn egl_dir_ready(provider: &Provider, root: &ArtRoot) -> bool {
    provider.egl_dir(root).is_dir()
}

/// Does the bound directory (the guest's `/vendor/lib64`) exist?
#[must_use]
pub fn lib_dir_ready(provider: &Provider, root: &ArtRoot) -> bool {
    provider.lib_dir(root).is_dir()
}

/// A one-line description for the run log.
#[must_use]
pub fn describe(provider: &Provider) -> String {
    let vulkan = match provider.vulkan_tag() {
        Some(tag) => format!(" vulkan={tag}"),
        None => String::new(),
    };
    format!(
        "gles: provider={} tag={}{vulkan} version={} licence={} source={}",
        provider.name,
        provider.manifest.tag,
        provider.manifest.version,
        provider.manifest.licence,
        provider.manifest.source
    )
}

/// The `ro.hardware.vulkan` value the selected provider stages, if any.
///
/// `Props::for_launch()` runs in several places that do not hold an [`ArtRoot`]
/// (main's `props print`, the session/app writers), so this resolves the root
/// the same way [`ArtRoot::resolve`] does. A missing sysroot is not an error
/// here: a run with the flag on fails loudly in `Sandbox::command` (the bind
/// source check) before any guest starts, and this is only the property table.
pub fn selected_vulkan_tag() -> Result<Option<String>> {
    let Some(name) = provider_name_from_env()? else {
        return Ok(None);
    };
    let root = match ArtRoot::resolve(None, None) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    let provider = load(&root, &name)?;
    Ok(provider.vulkan_tag().map(ToString::to_string))
}

/// Total staged bytes (the on-disk cost of the provider, shared by all runs).
#[must_use]
pub fn staged_bytes(provider: &Provider) -> u64 {
    provider.manifest.files.iter().map(|f| f.size).sum()
}

/// Path helper for tests/tools: `<sysroot>/gles` (or the override base).
#[must_use]
pub fn providers_root(root: &ArtRoot) -> PathBuf {
    match provider_dir_override() {
        Some(base) => base,
        None => root.sysroot.join("gles"),
    }
}

/// `<path>` relative to a provider dir, for the install tool's manifest.
#[must_use]
pub fn relative(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(dir: &Path) -> ArtRoot {
        ArtRoot {
            repo_root: dir.to_path_buf(),
            sysroot: dir.join("sysroot"),
            data: dir.join("data"),
            modules: Vec::new(),
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "acos-gles-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).expect("scratch");
        p
    }

    fn install(dir: &Path, name: &str, body: &[u8], tag: &str) -> Provider {
        install_with(dir, name, body, tag, None)
    }

    fn install_with(
        dir: &Path,
        name: &str,
        body: &[u8],
        tag: &str,
        vulkan: Option<(&str, &[u8])>,
    ) -> Provider {
        let base = dir.join("sysroot/gles").join(name);
        let egl = base.join("lib64/egl");
        fs::create_dir_all(&egl).expect("dirs");
        let f = egl.join(format!("libEGL_{tag}.so"));
        fs::write(&f, body).expect("write");
        let mut files = vec![GlesFile {
            path: "lib64/egl/".to_string() + &format!("libEGL_{tag}.so"),
            sha256: crate::build_cache::sha256_hex(body),
            size: body.len() as u64,
        }];
        let mut vulkan_tag = None;
        if let Some((vt, icd_body)) = vulkan {
            let hw = base.join("lib64/hw");
            fs::create_dir_all(&hw).expect("dirs");
            let icd = hw.join(format!("vulkan.{vt}.so"));
            fs::write(&icd, icd_body).expect("icd");
            files.push(GlesFile {
                path: format!("lib64/hw/vulkan.{vt}.so"),
                sha256: crate::build_cache::sha256_hex(icd_body),
                size: icd_body.len() as u64,
            });
            vulkan_tag = Some(vt.to_string());
        }
        let manifest = GlesManifest {
            tag: tag.to_string(),
            vulkan_tag,
            provider: "test".into(),
            source: "unit test".into(),
            version: "0".into(),
            licence: "Apache-2.0".into(),
            files,
        };
        fs::write(
            base.join(MANIFEST),
            serde_json::to_string_pretty(&manifest).expect("json"),
        )
        .expect("manifest");
        Provider {
            name: name.to_string(),
            manifest,
        }
    }

    #[test]
    fn no_flag_is_none_and_touches_nothing() {
        let dir = scratch("none");
        let r = root(&dir);
        std::env::remove_var(ENV);
        assert_eq!(selected(&r).expect("ok"), None);
    }

    #[test]
    fn a_verified_provider_names_its_egl_dir_and_tag() {
        let dir = scratch("ok");
        let r = root(&dir);
        let p = install(&dir, "angle", b"driver bytes", "angle");
        verify(&p, &r).expect("verified");
        assert_eq!(p.tag(), "angle");
        assert_eq!(
            p.egl_dir(&r),
            dir.join("sysroot/gles/angle/lib64/egl"),
            "findLibrary's directory is inside the bound one"
        );
        assert_eq!(
            p.lib_dir(&r),
            dir.join("sysroot/gles/angle/lib64"),
            "the bind source is the provider's lib64 (egl/ + the sonames)"
        );
        assert!(egl_dir_ready(&p, &r));
        assert_eq!(staged_bytes(&p), 12);
        assert!(describe(&p).contains("tag=angle"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_drifted_provider_fails_loudly() {
        let dir = scratch("stale");
        let r = root(&dir);
        let p = install(&dir, "angle", b"driver bytes", "angle");
        fs::write(p.egl_dir(&r).join("libEGL_angle.so"), b"DRIVER BYTES").expect("rewrite");
        let e = verify(&p, &r).expect_err("must fail");
        let text = e.to_string();
        assert!(text.contains("sha256"), "{text}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_provider_names_the_tool() {
        let dir = scratch("missing");
        let r = root(&dir);
        let e = load(&r, "nope").expect_err("must fail");
        assert!(e.to_string().contains("tools/art-gles.sh"), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_flag_rejects_path_shaped_names() {
        let dir = scratch("path");
        let r = root(&dir);
        for bad in ["../etc", "a/b", "..", "."] {
            std::env::set_var(ENV, bad);
            let e = selected(&r).expect_err(bad);
            assert!(e.to_string().contains("not a provider directory name"), "{e}");
        }
        std::env::remove_var(ENV);
    }

    #[test]
    fn a_manifest_without_files_or_tag_is_rejected() {
        let dir = scratch("bad");
        let r = root(&dir);
        let base = r.sysroot.join("gles/empty");
        fs::create_dir_all(&base).expect("dirs");
        fs::write(
            base.join(MANIFEST),
            r#"{"tag":"empty","provider":"x","source":"y","version":"0","licence":"z","files":[]}"#,
        )
        .expect("manifest");
        assert!(load(&r, "empty")
            .expect_err("no files")
            .to_string()
            .contains("no files"));
        fs::write(
            base.join(MANIFEST),
            r#"{"tag":"","provider":"x","source":"y","version":"0","licence":"z","files":[
                {"path":"a","sha256":"00","size":1}]}"#,
        )
        .expect("manifest");
        assert!(load(&r, "empty")
            .expect_err("no tag")
            .to_string()
            .contains("tag"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_manifest_without_the_tagged_driver_is_rejected() {
        let dir = scratch("wrongname");
        let r = root(&dir);
        let base = r.sysroot.join("gles/wrong");
        fs::create_dir_all(&base).expect("dirs");
        // The provider directory is `wrong`, the manifest claims `angle`, but
        // the staged file is `libEGL_other.so`: the loader would open nothing.
        fs::write(
            base.join(MANIFEST),
            r#"{"tag":"angle","provider":"x","source":"y","version":"0","licence":"z","files":[
                {"path":"lib64/egl/libEGL_other.so","sha256":"00","size":1}]}"#,
        )
        .expect("manifest");
        let e = load(&r, "wrong").expect_err("must fail");
        assert!(e.to_string().contains("lib64/egl/libEGL_angle.so"), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_vulkan_tag_without_its_icd_is_rejected() {
        let dir = scratch("noicd");
        let r = root(&dir);
        let base = r.sysroot.join("gles/angle-swiftshader");
        fs::create_dir_all(&base).expect("dirs");
        fs::write(
            base.join(MANIFEST),
            r#"{"tag":"angle","vulkan_tag":"pastel","provider":"x","source":"y",
                "version":"0","licence":"z","files":[
                {"path":"lib64/egl/libEGL_angle.so","sha256":"00","size":1}]}"#,
        )
        .expect("manifest");
        let e = load(&r, "angle-swiftshader").expect_err("must fail");
        assert!(e.to_string().contains("lib64/hw/vulkan.pastel.so"), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_provider_dir_name_is_a_label_and_the_tag_names_the_files() {
        let dir = scratch("label");
        let r = root(&dir);
        let p = install_with(
            &dir,
            "angle-swiftshader",
            b"driver bytes",
            "angle",
            Some(("pastel", b"icd bytes")),
        );
        assert_eq!(p.tag(), "angle", "the loader appends the *tag*, not the label");
        assert_eq!(p.vulkan_tag(), Some("pastel"));
        verify(&p, &r).expect("verified");
        assert_eq!(
            p.icd_path(&r).expect("icd"),
            dir.join("sysroot/gles/angle-swiftshader/lib64/hw/vulkan.pastel.so"),
            "the ICD lands in the same lib64 that is bound at /vendor/lib64"
        );
        assert!(p.icd_path(&r).expect("icd").is_file());
        let described = describe(&p);
        assert!(described.contains("tag=angle"), "{described}");
        assert!(described.contains("vulkan=pastel"), "{described}");
        assert_eq!(staged_bytes(&p), 12 + 9);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_drifted_icd_fails_loudly() {
        let dir = scratch("icd-stale");
        let r = root(&dir);
        let p = install_with(
            &dir,
            "angle-swiftshader",
            b"driver bytes",
            "angle",
            Some(("pastel", b"icd bytes")),
        );
        fs::write(p.icd_path(&r).expect("icd"), b"ICD BYTES").expect("rewrite");
        let e = verify(&p, &r).expect_err("must fail");
        assert!(e.to_string().contains("vulkan.pastel.so"), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }
}
