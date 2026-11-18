//! # acos-gles-provider — the guest's software GL provider, as a contract
//!
//! GLSHIP shipped software GL (ANGLE on SwiftShader) **on by default** for the
//! guest, GPUSPIKE added an opt-in host-GPU provider (`mesa-host`), and both
//! obey this module: one selection rule, one manifest, one verify cache.
//!
//! This crate is that contract, split out of `acos-art` so the three front
//! doors that report or apply it — `acos-art` (the launcher's bind),
//! `acos-cli` (`acos doctor`) and `acos-mcp` (`acos_capabilities`) — share one
//! implementation instead of `acos-mcp` pulling in the whole ART launcher for
//! one line of JSON. It depends on nothing but `serde`/`serde_json`.
//!
//! ## The name
//!
//! The brief calls this crate `acos-gles`. That name is already the guest-side
//! GLES2/EGL **substrate** (ADR-0025: `glsl.rs`, `raster.rs`, the JNI entry
//! points `acos-jni` installs), so this crate takes the qualified name rather
//! than renaming a shipped crate that `acos-jni`, `acos-runtime` and their
//! benches link. Renaming the substrate (it is a guest ABI surface, not a
//! provider helper) is a separate decision; if it happens, this crate can take
//! the short name with a `Cargo.toml` edit and one `use` per caller.
//!
//! ## What it is
//!
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
//! `Sandbox::command` bind-mounts the provider's directory **read-only** and
//! the property is a string in the per-run table. Neither maps a page of the
//! driver: `libEGL.so` is dlopened by libhwui when it first needs GL (the
//! RenderThread's EGL context), and only then does `Loader::open()` dlopen the
//! driver. A process that never touches EGL pays **zero** resident bytes (the
//! bind mount costs a mount entry, not memory) — measured in
//! `results/handoff/GLSPIKE/README.md` §5. When the flag is off this module is
//! never asked for anything: no bind, no property, no digest.
//!
//! ## Default ON when installed (GLSHIP)
//!
//! `ACOS_GUEST_GLES` selects the provider:
//!
//! * unset/empty — the **default**, [`DEFAULT_PROVIDER`]
//!   (`angle-swiftshader`), when `<sysroot>/gles/angle-swiftshader` is
//!   installed. When it is not installed the run is exactly today's guest: no
//!   bind, no property, no digest, libEGL keeps its own fatal.
//! * `off` — no provider, whatever is installed (the escape hatch).
//! * any other name — that provider, and it **must** be installed: an explicit
//!   request for bytes that are not there is a structured `Error::Layout`,
//!   never a silent fall back to the default.
//!   `mesa-host` is reserved for the GPU track's provider and is only ever
//!   selected explicitly (nothing ships it today; the image build stages only
//!   `angle-swiftshader`, and no code path selects `mesa-host` on its own).
//!
//! ## GPUSPIKE: a host-GPU provider is opt-in, and names its one device
//!
//! A provider may declare two extra manifest fields, both of which only matter
//! when that provider is the selected one:
//!
//! * `props` — extra guest system properties the provider needs (Waydroid's
//!   `ro.hardware.gralloc=gbm`, `gralloc.gbm.device=/dev/dri/renderD129`).
//!   [`valid_prop_key`] rejects `ro.hardware.egl`/`ro.hardware.vulkan` (those
//!   are derived from `tag`/`vulkan_tag`, one spelling) and any non-dotted-ASCII
//!   key, so a hand-edited manifest cannot rewrite the derivation.
//! * `render_node` — the host DRM **render** node (`/dev/dri/renderD<N>`), bound
//!   read-write into the sandbox only when this provider is selected
//!   ([`Provider::render_node`]). [`valid_render_node`] is the whole grammar: no
//!   card nodes (modesetting/master), no other `/dev` path.
//!
//! Selection stays the single rule in [`status`]: an uninstalled provider is a
//! structured error, never a fall back to the default, so `mesa-host` is only
//! ever reached by naming it. Its bytes are verified through the same cache as
//! the default provider ([`verify`]), so a host-GPU run pays the same one-time
//! digest and nothing per launch.
//!
//! The provider directory carries a `manifest.json` (source, version, licence,
//! sha256 per file) written by `tools/art-gles.sh`; the launcher verifies every
//! file's sha256 against it (the A11YPACE stale-shim lesson: a shared prebuilt
//! that silently predates its contract must fail loudly). A mismatched provider
//! is a structured `Error::Layout`, never a silent skip that would reach
//! libEGL's fatal assert instead.
//!
//! The digest is **cached per host, next to the provider** ([`verify`],
//! [`VERIFIED`]): the first launch after a (re)stage hashes 32 MB and writes
//! `<provider>/.verified`; every later launch compares the cheap stat
//! fingerprint — each file's `(dev, ino, size, mtime)` plus the manifest the
//! caller holds — and hashes nothing. Any change to that fingerprint (a
//! re-stage, a touched file, a different manifest) re-hashes; a missing or
//! unreadable file is the same structured `Error::Layout` as before. The
//! provider directory is published atomically by `tools/art-gles.sh`, so a
//! `.verified` file can never describe a half-written provider.
//!
//! Accepted risk (documented, owner decision): a file rewritten **in place with
//! the same size and the same `mtime`** on the same inode is not re-hashed, so
//! it would be trusted. Nothing in ACOS writes a staged provider that way
//! (`tools/art-gles.sh` replaces the directory), and `mtime`/`ino`/`size` are
//! the standard make-style freshness contract; the alternative — hashing 32 MB
//! on every launch — is a measurable launch cost for a case that cannot arise
//! from the shipped tooling.
//!
//! Licence/source are part of the manifest, not of this module: the spike ships
//! no GL code, only the mechanism to stage someone else's.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The provider contract's only failure: a structured, actionable layout error
/// (a missing provider, a manifest that disagrees with the staged bytes, a
/// render node that is not a device). Every caller maps it onto its own error
/// type; nothing here is a silent fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// `ACOS_GUEST_GLES` selection or a staged provider that does not satisfy
    /// the contract. The message always names the variable and the path.
    Layout(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Layout(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

/// `Result` with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// The environment variable that selects the provider. Absent/empty means the
/// default provider when it is installed.
pub const ENV: &str = "ACOS_GUEST_GLES";

/// The provider selected when [`ENV`] is unset: the software GL the image
/// ships (ANGLE on SwiftShader's CPU Vulkan ICD). `tools/art-gles.sh` stages it
/// at `<sysroot>/gles/angle-swiftshader` as part of the sysroot build.
pub const DEFAULT_PROVIDER: &str = "angle-swiftshader";

/// The [`ENV`] value that disables software GL (`ACOS_GUEST_GLES=off`).
pub const OFF: &str = "off";

/// The [`ENV`] value reserved for the GPU track's host-Mesa provider. Never
/// selected implicitly: it is not installed by the image build, so asking for
/// it without staging it is a structured error, not the default.
pub const RESERVED_MESA_HOST: &str = "mesa-host";

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
    /// (`lib64/hw/vulkan.<vulkan_tag>.so` **and** the same file at
    /// `lib64/vulkan.<vulkan_tag>.so`, see [`Provider::icd_path`]), e.g.
    /// `pastel` for SwiftShader.
    ///
    /// GLSPIKE2: ANGLE's Vulkan backend is only as good as the ICD the image's
    /// `libvulkan.so` can find, and `libvulkan.so` names that ICD from this
    /// property (`hwvulkan module open`, `vulkan.<tag>`, measured in the
    /// sysroot's own libvulkan). `None` = no ICD, i.e. GLSPIKE's arm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vulkan_tag: Option<String>,
    /// Extra guest system properties the provider needs (GPUSPIKE): Waydroid's
    /// `ro.hardware.gralloc=gbm`, `gralloc.gbm.device=/dev/dri/renderD129`.
    /// Never `ro.hardware.egl`/`ro.hardware.vulkan` (those come from `tag` and
    /// `vulkan_tag`, one spelling); see [`valid_prop_key`].
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub props: std::collections::BTreeMap<String, String>,
    /// The host DRM render node a host-GPU provider needs inside the sandbox
    /// (`/dev/dri/renderD<N>`), bound read-write **only** when this provider
    /// is selected. `$ACOS_GUEST_GLES_RENDER_NODE` overrides it (the node
    /// number differs per machine and boot). See [`Provider::render_node`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_node: Option<String>,
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
    pub fn dir(&self, sysroot: &Path) -> PathBuf {
        if let Some(dir) = provider_dir_override() {
            return dir.join(&self.name);
        }
        sysroot.join("gles").join(&self.name)
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

    /// `<provider>/lib64/hw/vulkan.<vulkan_tag>.so`, the ICD's canonical
    /// (HAL) path. It lives in the same `lib64/` the launcher binds at
    /// `/vendor/lib64`, so no second bind is needed.
    ///
    /// GLSPIKE4 (measured, `results/handoff/GLSPIKE4/README.md`): the image's
    /// `libvulkan.so` does **not** open that path. It calls libvndksupport's
    /// `android_load_sphal_library("vulkan.<tag>.so")` with the **bare name**,
    /// which in a guest with no `sphal` namespace is a default-namespace
    /// `dlopen` over `namespace.default.search.paths` — `/vendor/lib64`, not
    /// `/vendor/lib64/hw`. The same bytes therefore also have to exist at
    /// [`Provider::icd_soname_path`] or `vkCreateInstance` fails and ANGLE's
    /// `eglInitialize` reports `EGL_NOT_INITIALIZED` without ever mapping the
    /// ICD.
    #[must_use]
    pub fn icd_path(&self, sysroot: &Path) -> Option<PathBuf> {
        let tag = self.vulkan_tag()?;
        Some(self.lib_dir(sysroot).join("hw").join(format!("vulkan.{tag}.so")))
    }

    /// `<provider>/lib64/vulkan.<vulkan_tag>.so`: the ICD by the bare name the
    /// guest's default-namespace `dlopen` can resolve (a hardlink of
    /// [`Provider::icd_path`], one inode).
    #[must_use]
    pub fn icd_soname_path(&self, sysroot: &Path) -> Option<PathBuf> {
        let tag = self.vulkan_tag()?;
        Some(self.lib_dir(sysroot).join(format!("vulkan.{tag}.so")))
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
    pub fn lib_dir(&self, sysroot: &Path) -> PathBuf {
        self.dir(sysroot).join("lib64")
    }

    /// The provider's `lib64/egl` (what libEGL's `findLibrary` searches).
    #[must_use]
    pub fn egl_dir(&self, sysroot: &Path) -> PathBuf {
        self.lib_dir(sysroot).join("egl")
    }

    /// The host render node to bind, if this provider needs the GPU: the env
    /// override, else the manifest's; validated either way. `Err` when the
    /// value is not a render node or the node does not exist on this host.
    pub fn render_node(&self) -> Result<Option<PathBuf>> {
        let raw = match std::env::var(RENDER_NODE_ENV) {
            Ok(v) if !v.trim().is_empty() => Some(v.trim().to_string()),
            _ => self.manifest.render_node.clone(),
        };
        let Some(node) = raw else {
            return Ok(None);
        };
        if !valid_render_node(&node) {
            return Err(Error::Layout(format!(
                "ACOS_GUEST_GLES={}: render node {node:?} is not /dev/dri/renderD<N>",
                self.name
            )));
        }
        let path = PathBuf::from(&node);
        if !path.exists() {
            return Err(Error::Layout(format!(
                "ACOS_GUEST_GLES={}: render node {node} does not exist on this host \
                 (set {RENDER_NODE_ENV}=/dev/dri/renderD<N> for this machine's GPU)",
                self.name
            )));
        }
        Ok(Some(path))
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

/// The environment variable that overrides the manifest's `render_node`.
pub const RENDER_NODE_ENV: &str = "ACOS_GUEST_GLES_RENDER_NODE";

/// `/dev/dri/renderD<digits>` and nothing else: the only device the flag can
/// ever bind into the sandbox (no card nodes = no modesetting/master, no other
/// `/dev` path), so a hand-edited manifest or env cannot widen the sandbox.
#[must_use]
pub fn valid_render_node(path: &str) -> bool {
    match path.strip_prefix("/dev/dri/renderD") {
        Some(n) => !n.is_empty() && n.len() <= 4 && n.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

/// A property key a manifest may set: dotted ASCII, and never the two the
/// launcher derives from `tag`/`vulkan_tag`.
#[must_use]
pub fn valid_prop_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() < 32
        && key != "ro.hardware.egl"
        && key != "ro.hardware.vulkan"
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Which provider a run uses, and why (the `acos doctor` / capabilities line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// The provider to bind, or `None` for today's guest (no GL).
    pub provider: Option<Provider>,
    /// True when the provider was chosen implicitly (the default).
    pub default: bool,
    /// One-line explanation, always set.
    pub detail: String,
}

impl Status {
    /// The `gles` capability label: the provider name, or `none`.
    #[must_use]
    pub fn label(&self) -> &str {
        match &self.provider {
            Some(p) => &p.name,
            None => "none",
        }
    }
}

/// Resolve the provider for this run: the flag, else the installed default.
///
/// This is the one selection rule (the launcher's bind, the property table and
/// `acos doctor` all call it), so "installed" cannot mean two things.
pub fn status(sysroot: &Path) -> Result<Status> {
    if let Some(name) = provider_name_from_env()? {
        if name.eq_ignore_ascii_case(OFF) {
            return Ok(Status {
                provider: None,
                default: false,
                detail: format!("ACOS_GUEST_GLES={OFF}"),
            });
        }
        // An explicit name must exist: silently serving the default instead
        // would run a different driver than the report claims.
        let provider = load(sysroot, &name)?;
        let detail = format!("ACOS_GUEST_GLES={name}");
        return Ok(Status {
            provider: Some(provider),
            default: false,
            detail,
        });
    }
    let dir = providers_root(sysroot).join(DEFAULT_PROVIDER);
    if !dir.join(MANIFEST).is_file() {
        return Ok(Status {
            provider: None,
            default: true,
            detail: format!(
                "{DEFAULT_PROVIDER} is not installed at {} (stage it with \
                 `tools/art-gles.sh`, or the image build)",
                dir.display()
            ),
        });
    }
    let provider = load(sysroot, DEFAULT_PROVIDER)?;
    let detail = format!("default; installed at {}", dir.display());
    Ok(Status {
        provider: Some(provider),
        default: true,
        detail,
    })
}

/// Read `ACOS_GUEST_GLES` and load/verify the provider it names.
///
/// Returns `Ok(None)` when software GL is off — explicitly
/// (`ACOS_GUEST_GLES=off`) or because the default provider is not installed
/// (the guest then keeps today's behaviour and libEGL's fatal is unchanged).
pub fn selected(sysroot: &Path) -> Result<Option<Provider>> {
    Ok(status(sysroot)?.provider)
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
///
/// This is the manifest **tag**, never the `ACOS_GUEST_GLES` value: the loader
/// appends it to `libEGL_`, and the provider directory is only a label (a
/// provider called `angle-swiftshader` stages `libEGL_angle.so`). Serving the
/// directory name instead makes the loader look for `libEGL_angle-swiftshader.so`,
/// find nothing, and abort with its own `couldn't find an OpenGL ES
/// implementation` fatal — measured on ART (results/handoff/GLSPIKE2).
pub fn tag_from_env(sysroot: &Path) -> Result<Option<String>> {
    Ok(selected(sysroot)?.map(|p| p.manifest.tag))
}

/// The `(ro.hardware.egl, ro.hardware.vulkan)` values the **selected**
/// provider declares, or `None` when software GL is off.
///
/// The caller supplies the guest sysroot: `acos-art` has an `ArtRoot`, and
/// `acos-cli`/`acos-mcp` resolve the same directory through their own runtime
/// root (`ACOS_ART_SYSROOT`, then `<root>/.cache/art/sysroot`) — the rule lives
/// with the caller so this crate stays a pure function of a path and the
/// `ACOS_GUEST_GLES*` variables. A missing sysroot is not an error here: a run
/// with a provider selected fails loudly in `Sandbox::command` (the bind source
/// check) before any guest starts, and this is only the property table.
pub fn selected_tags_for(sysroot: &Path) -> Result<Option<(String, Option<String>)>> {
    Ok(selected_props_for(sysroot)?.map(|(egl, vulkan, _)| (egl, vulkan)))
}

/// [`selected_tags_for`] plus the provider's extra properties (GPUSPIKE:
/// Waydroid's `ro.hardware.gralloc`/`gralloc.gbm.device`), in the same one
/// selection rule.
pub fn selected_props_for(
    sysroot: &Path,
) -> Result<Option<(String, Option<String>, Vec<(String, String)>)>> {
    Ok(status(sysroot)?.provider.map(|provider| {
        (
            provider.manifest.tag.clone(),
            provider.vulkan_tag().map(ToString::to_string),
            provider
                .manifest
                .props
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        )
    }))
}

/// The `gles` capability line for `acos doctor` / `acos_capabilities`:
/// `angle-swiftshader` when a provider is selected (the default one, or the
/// `ACOS_GUEST_GLES` override), else `none`, plus the explanation.
///
/// `sysroot` is the caller's resolved guest sysroot, or `None` when it has none
/// (nothing installed, an unreadable root): the report then says `none` and
/// why. Never an error — the launch path is where a broken explicit selection
/// must fail loudly.
#[must_use]
pub fn capability_for(sysroot: Option<&Path>) -> (String, String) {
    let Some(sysroot) = sysroot else {
        return (
            "none".to_string(),
            format!(
                "no guest sysroot (set ACOS_ART_SYSROOT, or stage one at \
                 <root>/.cache/art/sysroot); {DEFAULT_PROVIDER} is not installed"
            ),
        );
    };
    match status(sysroot) {
        Ok(s) => (s.label().to_string(), s.detail),
        Err(e) => ("none".to_string(), e.to_string()),
    }
}

/// Load and parse `<data>/gles/<name>/manifest.json` (no hashing; [`verify`]
/// checks the bytes).
pub fn load(sysroot: &Path, name: &str) -> Result<Provider> {
    let dir = match provider_dir_override() {
        Some(base) => base.join(name),
        None => providers_root(sysroot).join(name),
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
    // GPUSPIKE: the two host-GPU fields are validated at load, so a hand-edited
    // manifest can never widen the sandbox (device grammar, derived props).
    for k in manifest.props.keys() {
        if !valid_prop_key(k) {
            return Err(Error::Layout(format!(
                "ACOS_GUEST_GLES={name}: manifest prop key {k:?} is not allowed"
            )));
        }
    }
    if let Some(node) = manifest.render_node.as_deref() {
        if !valid_render_node(node) {
            return Err(Error::Layout(format!(
                "ACOS_GUEST_GLES={name}: manifest render_node {node:?} is not /dev/dri/renderD<N>"
            )));
        }
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
        // Both names: the HAL path and the bare soname the guest's default
        // namespace resolves (GLSPIKE4; see `Provider::icd_path`).
        for icd in [
            format!("lib64/hw/vulkan.{vt}.so"),
            format!("lib64/vulkan.{vt}.so"),
        ] {
            if !manifest.files.iter().any(|f| f.path == icd) {
                return Err(Error::Layout(format!(
                    "ACOS_GUEST_GLES={name}: manifest vulkan_tag is {vt:?} but it stages no {icd} \
                     (reinstall with `tools/art-gles.sh`)"
                )));
            }
        }
    }
    Ok(Provider {
        name: name.to_string(),
        manifest,
    })
}

/// The verified fingerprint of a staged provider: `<provider>/.verified`.
///
/// Next to the provider it describes, so it travels with it: one file per
/// provider **tag** (`angle-swiftshader` now, `mesa-host` when the GPU track
/// stages it), shared by every run on the host, and inside the install sysroot
/// when ACOS is installed (the sysroot is copied whole). Only `lib64/` is bound
/// into the guest, so the guest never sees it.
pub const VERIFIED: &str = ".verified";

/// `<provider>/.verified` (see [`VERIFIED`]).
#[must_use]
pub fn verify_cache_path(provider: &Provider, sysroot: &Path) -> PathBuf {
    provider.dir(sysroot).join(VERIFIED)
}

/// The identity of the staged bytes that a launch verifies: the manifest's own
/// digest plus each file's `(dev, ino, size, mtime, ctime)`.
///
/// Content changes through the filesystem update `mtime`/`ctime` and a re-stage
/// replaces the inode, so a match means "the bytes I hashed are still the bytes
/// on disk" without reading them. `dev`/`ino` keep it correct when the same
/// path is replaced by a different file that happens to share a timestamp.
fn signature(provider: &Provider, sysroot: &Path) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    let dir = provider.dir(sysroot);
    // The manifest **as the caller holds it**, not the bytes on disk: a caller
    // that verifies against a manifest other than the staged one (a test, a
    // re-serialised copy) must not inherit a cache entry made for a different
    // contract.
    let manifest = serde_json::to_string(&provider.manifest).unwrap_or_default();
    let mut text = format!(
        "v1 {} {}\n",
        provider.name,
        crate::sha256_hex(manifest.as_bytes())
    );
    for f in &provider.manifest.files {
        let path = dir.join(&f.path);
        let meta = fs::metadata(&path).map_err(|e| {
            Error::Layout(format!(
                "ACOS_GUEST_GLES={}: {} is missing ({e})",
                provider.name,
                path.display()
            ))
        })?;
        // `dev`/`ino` catch a replacement of the same path, `mtime`/`size` a
        // rewrite in place. `ctime` is deliberately *not* used: `chmod` bumps it
        // and does not change the bytes, so it would only cause re-hashing.
        text.push_str(&format!(
            "{} {} {} {} {}.{}\n",
            f.path,
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec()
        ));
    }
    Ok(crate::sha256_hex(text.as_bytes()))
}

/// Verify the staged files, hashing them only when the stat fingerprint moved.
///
/// Called once per launch (default on). The first launch after a stage pays the
/// digest (81 ms for the 32 MB ANGLE+SwiftShader provider, measured in
/// `results/handoff/GLSPIKE4/README.md` §6); every later launch pays 8 `stat`s
/// and one small read of [`VERIFIED`] (measured in
/// `results/handoff/GLSHIP/README.md` §5). A changed fingerprint re-hashes; a
/// missing or unreadable file is the structured error below, cache or not. The
/// cache is best effort: if it cannot be written (a read-only provider) the
/// launch still verifies, it just hashes every time.
pub fn verify(provider: &Provider, sysroot: &Path) -> Result<()> {
    let sig = signature(provider, sysroot)?;
    let cache = verify_cache_path(provider, sysroot);
    if fs::read_to_string(&cache)
        .map(|cached| cached.trim() == sig)
        .unwrap_or(false)
    {
        return Ok(());
    }
    verify_full(provider, sysroot)?;
    // Atomic publish: a concurrent launch may be reading it. `rename` inside the
    // provider directory keeps the write off the read path of every other run.
    let tmp = cache.with_extension(format!("tmp.{}", std::process::id()));
    if fs::write(&tmp, format!("{sig}\n")).is_ok() && fs::rename(&tmp, &cache).is_err() {
        let _ = fs::remove_file(&tmp);
    }
    Ok(())
}

/// Verify every staged file against the manifest's sha256 and size, always.
///
/// A mismatch is a hard error: the whole point of shipping the provider is that
/// a *known* driver is loaded, so a shared prebuilt that drifted cannot reach
/// libEGL as a different library than the report describes.
pub fn verify_full(provider: &Provider, sysroot: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let dir = provider.dir(sysroot);
    let mut hashed: std::collections::HashMap<(u64, u64), String> =
        std::collections::HashMap::new();
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
        // The provider stages every driver (and the ICD) under two names that
        // are hardlinks of one inode (`lib64/` + `lib64/egl/`, `lib64/hw/`).
        // Hash each inode once: same inode, same bytes, and the second name is
        // still compared against its own manifest entry below.
        let key = (meta.dev(), meta.ino());
        let got = match hashed.get(&key) {
            Some(h) => h.clone(),
            None => {
                let bytes = fs::read(&path).map_err(|e| {
                    Error::Layout(format!(
                        "ACOS_GUEST_GLES={}: {} is unreadable ({e})",
                        provider.name,
                        path.display()
                    ))
                })?;
                let h = crate::sha256_hex(&bytes);
                hashed.insert(key, h.clone());
                h
            }
        };
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
pub fn egl_dir_ready(provider: &Provider, sysroot: &Path) -> bool {
    provider.egl_dir(sysroot).is_dir()
}

/// Does the bound directory (the guest's `/vendor/lib64`) exist?
#[must_use]
pub fn lib_dir_ready(provider: &Provider, sysroot: &Path) -> bool {
    provider.lib_dir(sysroot).is_dir()
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

/// Total staged bytes (the on-disk cost of the provider, shared by all runs).
#[must_use]
pub fn staged_bytes(provider: &Provider) -> u64 {
    provider.manifest.files.iter().map(|f| f.size).sum()
}

/// Path helper for tests/tools: `<sysroot>/gles` (or the override base).
#[must_use]
pub fn providers_root(sysroot: &Path) -> PathBuf {
    match provider_dir_override() {
        Some(base) => base,
        None => sysroot.join("gles"),
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


// ---- sha256 -----------------------------------------------------------------
//
// The manifest is a sha256 contract, so the crate needs a digest. It is the
// same 60-line implementation `acos-art::build_cache` carries (that one now
// delegates here), `acos-verify` and `acos-render` keep their own: a shared
// `acos-hash` crate is the next step, and this one at least keeps the provider
// contract self-contained (no dependency on the ART launcher).

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// sha256 of `data`, lowercase hex.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (msg.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for (i, k) in K.iter().enumerate() {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k)
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    hex(&out)
}

fn hex(bytes: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(D[(b >> 4) as usize] as char);
        s.push(D[(b & 0xf) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The process environment is global: every test that mutates it takes
    /// this lock, so one test's `ACOS_GUEST_GLES`/`..._DIR`/`..._RENDER_NODE`
    /// cannot leak into another's (they run on parallel threads).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The guest sysroot a scratch root holds: `<dir>/sysroot`.
    fn root(dir: &Path) -> PathBuf {
        dir.join("sysroot")
    }

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "acos-gles-provider-{tag}-{}-{:?}",
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

    /// The property-table view of a scratch root (what `Props::for_launch`
    /// would serve), without touching the process-wide root resolution.
    fn props_view_for(r: &Path) -> Option<(String, Option<String>)> {
        status(r).expect("status").provider.map(|p| {
            (
                p.manifest.tag.clone(),
                p.vulkan_tag().map(ToString::to_string),
            )
        })
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
            sha256: crate::sha256_hex(body),
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
                sha256: crate::sha256_hex(icd_body),
                size: icd_body.len() as u64,
            });
            fs::write(base.join(format!("lib64/vulkan.{vt}.so")), icd_body).expect("icd soname");
            files.push(GlesFile {
                path: format!("lib64/vulkan.{vt}.so"),
                sha256: crate::sha256_hex(icd_body),
                size: icd_body.len() as u64,
            });
            vulkan_tag = Some(vt.to_string());
        }
        let manifest = GlesManifest {
            tag: tag.to_string(),
            vulkan_tag,
            props: std::collections::BTreeMap::new(),
            render_node: None,
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
    fn an_uninstalled_default_is_none_and_touches_nothing() {
        let _guard = env_lock();
        let dir = scratch("none");
        let r = root(&dir);
        std::env::remove_var(ENV);
        std::env::remove_var(DIR_ENV);
        // GLSHIP: software GL is ON by default, but only when the provider is
        // installed. Nothing is installed in this scratch root, so the run is
        // exactly today's guest: no provider, and no error.
        assert_eq!(selected(&r).expect("ok"), None);
        let s = status(&r).expect("status");
        assert_eq!(s.label(), "none");
        assert!(s.detail.contains(DEFAULT_PROVIDER), "{}", s.detail);
        assert!(s.detail.contains("not installed"), "{}", s.detail);
        assert!(!s.detail.contains("none ("), "the label is not repeated: {}", s.detail);
        let _ = fs::remove_dir_all(&dir);
    }

    /// GLSHIP: the shipped provider is the default, with no flag set at all.
    #[test]
    fn the_default_provider_is_selected_when_it_is_installed() {
        let _guard = env_lock();
        let dir = scratch("default");
        let r = root(&dir);
        std::env::remove_var(ENV);
        std::env::remove_var(DIR_ENV);
        let p = install_with(
            &dir,
            DEFAULT_PROVIDER,
            b"driver bytes",
            "angle",
            Some(("pastel", b"icd bytes")),
        );
        let s = status(&r).expect("status");
        assert_eq!(s.provider.as_ref(), Some(&p), "default = angle-swiftshader");
        assert!(s.default, "chosen implicitly");
        assert_eq!(s.label(), DEFAULT_PROVIDER);
        assert_eq!(props_view_for(&r), Some(("angle".into(), Some("pastel".into()))));
        verify(&p, &r).expect("verified");
        let _ = fs::remove_dir_all(&dir);
    }

    /// GLSHIP: `ACOS_GUEST_GLES=off` is the escape hatch — even with the
    /// provider installed, the run gets today's guest (no bind, no props).
    #[test]
    fn off_disables_the_default() {
        let _guard = env_lock();
        let dir = scratch("off");
        let r = root(&dir);
        std::env::remove_var(DIR_ENV);
        install(&dir, DEFAULT_PROVIDER, b"driver bytes", "angle");
        std::env::set_var(ENV, "off");
        let s = status(&r).expect("status");
        assert_eq!(s.provider, None);
        assert!(s.detail.contains("ACOS_GUEST_GLES=off"), "{}", s.detail);
        std::env::set_var(ENV, "OFF");
        assert_eq!(selected(&r).expect("ok"), None, "case-insensitive");
        std::env::remove_var(ENV);
        let _ = fs::remove_dir_all(&dir);
    }

    /// GLSHIP: an explicitly named provider that is not installed is a loud
    /// error, never a silent fall back to the default (that would run a
    /// different driver than the report names). `mesa-host` is the GPU track's
    /// reserved name: selecting it before it is staged says so.
    #[test]
    fn an_explicit_missing_provider_is_an_error_not_the_default() {
        let _guard = env_lock();
        let dir = scratch("explicit-missing");
        let r = root(&dir);
        std::env::remove_var(DIR_ENV);
        install(&dir, DEFAULT_PROVIDER, b"driver bytes", "angle");
        std::env::set_var(ENV, RESERVED_MESA_HOST);
        let e = selected(&r).expect_err("explicit miss must fail");
        assert!(e.to_string().contains(RESERVED_MESA_HOST), "{e}");
        std::env::remove_var(ENV);
        assert!(
            selected(&r).expect("default ok").is_some(),
            "and the default is untouched by it"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// GLSHIP: the digest is paid once per staged fingerprint, not per launch.
    ///
    /// Proven by removing read permission after the first verify: a cache hit
    /// needs only `stat`, so it still succeeds; any change to the fingerprint
    /// forces the full read again and fails. The second launch is also
    /// *measured* against a full digest of the same bytes, so "no hashing" is a
    /// number and not only an absence of an error.
    #[test]
    fn the_digest_is_cached_until_the_fingerprint_changes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("verify-cache");
        let r = root(&dir);
        // 8 MiB so the digest is measurable, not a rounding error.
        let body = vec![0xA5u8; 8 << 20];
        let p = install(&dir, "angle", &body, "angle");
        let cache = verify_cache_path(&p, &r);
        assert_eq!(cache, p.dir(&r).join(VERIFIED), "next to the provider");
        assert!(!cache.exists(), "nothing verified yet");
        verify(&p, &r).expect("first verify hashes");
        assert!(
            cache.is_file(),
            "the fingerprint is recorded at {}",
            cache.display()
        );
        let file = p.egl_dir(&r).join("libEGL_angle.so");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).expect("chmod");
        let cached = std::time::Instant::now();
        verify(&p, &r).expect("cache hit: the bytes are not read again");
        let cached_us = cached.elapsed().as_micros();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod");
        // The full path on the same bytes, for the cost the cache avoids.
        let full = std::time::Instant::now();
        verify_full(&p, &r).expect("the bytes are intact");
        let full_us = full.elapsed().as_micros();
        assert!(
            cached_us * 10 < full_us,
            "cache hit {cached_us}us must be an order of magnitude below a full \
             digest of the same 8 MiB ({full_us}us)"
        );
        // A re-stage replaces the inode; the same path with new content must be
        // hashed again (and here: fail, because the file cannot be read).
        fs::remove_file(&file).expect("rm");
        fs::write(&file, b"DRIVER BYTES").expect("rewrite");
        let e = verify(&p, &r).expect_err("new inode, new digest");
        assert!(e.to_string().contains("libEGL_angle.so"), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// GLSHIP: a **touched** file (same bytes, new `mtime`) re-hashes, so a
    /// provider that was replaced in place cannot ride on a stale fingerprint.
    #[test]
    fn a_touched_file_is_hashed_again() {
        let dir = scratch("verify-touched");
        let r = root(&dir);
        let p = install(&dir, "angle", b"driver bytes", "angle");
        verify(&p, &r).expect("first verify");
        let file = p.egl_dir(&r).join("libEGL_angle.so");
        // Same bytes, later mtime: the fingerprint moves, the digest must run.
        set_mtime_ns(&file, mtime_ns(&file) + 1_000_000_000);
        fs::write(&file, b"DRIVER BYTES").expect("rewrite in place");
        set_mtime_ns(&file, mtime_ns(&file) + 2_000_000_000);
        let e = verify(&p, &r).expect_err("touched + changed bytes must fail");
        assert!(e.to_string().contains("sha256"), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// GLSHIP accepted risk (documented in the module docs): bytes rewritten in
    /// place with the **same size and the same `mtime`** are trusted, because
    /// the fingerprint matches. The provider is published atomically by
    /// `tools/art-gles.sh`, so this cannot arise from the shipped tooling — and
    /// hashing 32 MB on every launch is a real cost for a case that cannot
    /// happen. This test pins the contract so a future change to it is a
    /// decision, not an accident.
    #[test]
    fn same_size_and_mtime_is_the_documented_accepted_risk() {
        let dir = scratch("verify-same-mtime");
        let r = root(&dir);
        let p = install(&dir, "angle", b"driver bytes", "angle");
        verify(&p, &r).expect("first verify");
        let file = p.egl_dir(&r).join("libEGL_angle.so");
        let mtime = mtime_ns(&file);
        // Same length, different bytes, mtime restored to the verified value.
        fs::write(&file, b"DRIVER BYTEZ").expect("rewrite in place");
        set_mtime_ns(&file, mtime);
        verify(&p, &r).expect("same fingerprint: trusted (accepted risk)");
        // ... and the moment the fingerprint moves, it is caught.
        set_mtime_ns(&file, mtime + 1_000_000_000);
        let e = verify(&p, &r).expect_err("a new mtime re-hashes");
        assert!(e.to_string().contains("sha256"), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }

    fn mtime_ns(path: &Path) -> i64 {
        use std::os::unix::fs::MetadataExt;
        let m = fs::metadata(path).expect("metadata");
        m.mtime() * 1_000_000_000 + m.mtime_nsec()
    }

    /// Set the modification time (atime untouched), so a test can express
    /// "rewritten in place with the same `mtime`" without a new dependency.
    fn set_mtime_ns(path: &Path, ns: i64) {
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("path");
        let times = [
            libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_OMIT,
            },
            libc::timespec {
                tv_sec: ns / 1_000_000_000,
                tv_nsec: ns % 1_000_000_000,
            },
        ];
        let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(rc, 0, "utimensat failed for {}", path.display());
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
        let base = r.join("gles/empty");
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
        let base = r.join("gles/wrong");
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
        let base = r.join("gles/angle-swiftshader");
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

    /// GLSPIKE4: a provider that stages the ICD only at the HAL path
    /// (`lib64/hw/vulkan.<tag>.so`, GLSPIKE2's layout) is rejected: the guest's
    /// `libvulkan.so` opens the **bare** `vulkan.<tag>.so`, which only exists in
    /// the default namespace's search path if the file is also at `lib64/`.
    /// Before the fix this manifest loaded and the guest failed `eglInitialize`
    /// with `Could not load vulkan.pastel.so from default namespace`.
    #[test]
    fn an_icd_staged_only_at_the_hal_path_is_rejected() {
        let dir = scratch("icd-hw-only");
        let r = root(&dir);
        let base = r.join("gles/angle-swiftshader");
        fs::create_dir_all(&base).expect("dirs");
        fs::write(
            base.join(MANIFEST),
            r#"{"tag":"angle","vulkan_tag":"pastel","provider":"x","source":"y",
                "version":"0","licence":"z","files":[
                {"path":"lib64/egl/libEGL_angle.so","sha256":"00","size":1},
                {"path":"lib64/hw/vulkan.pastel.so","sha256":"00","size":1}]}"#,
        )
        .expect("manifest");
        let e = load(&r, "angle-swiftshader").expect_err("must fail");
        assert!(e.to_string().contains("lib64/vulkan.pastel.so"), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_icd_soname_path_is_the_bare_name_in_the_bound_lib_dir() {
        let dir = scratch("icd-soname");
        let r = root(&dir);
        let p = install_with(
            &dir,
            "angle-swiftshader",
            b"driver bytes",
            "angle",
            Some(("pastel", b"icd bytes")),
        );
        let soname = p.icd_soname_path(&r).expect("soname");
        assert_eq!(
            soname,
            dir.join("sysroot/gles/angle-swiftshader/lib64/vulkan.pastel.so"),
            "bound at /vendor/lib64, which is in namespace.default.search.paths"
        );
        assert!(soname.is_file());
        // no ICD, no soname path
        let q = install(&dir, "angle", b"d", "angle");
        assert!(q.icd_soname_path(&r).is_none());
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
        assert_eq!(staged_bytes(&p), 12 + 9 + 9);
        let _ = fs::remove_dir_all(&dir);
    }

    /// GLSPIKE4: the two names of a hardlinked file are hashed once, but each
    /// name is still checked against its own manifest entry.
    #[test]
    fn hardlinked_names_are_hashed_once_and_each_checked() {
        let dir = scratch("hardlinks");
        let r = root(&dir);
        let p = install_with(
            &dir,
            "angle-swiftshader",
            b"driver bytes",
            "angle",
            Some(("pastel", b"icd bytes")),
        );
        let hw = p.icd_path(&r).expect("icd");
        let soname = p.icd_soname_path(&r).expect("soname");
        fs::remove_file(&soname).expect("rm");
        fs::hard_link(&hw, &soname).expect("link");
        verify(&p, &r).expect("one inode, both entries agree");
        // a manifest whose second name claims other bytes must still fail
        let mut drifted = p.clone();
        for f in &mut drifted.manifest.files {
            if f.path.ends_with("lib64/vulkan.pastel.so") {
                f.sha256 = "00".into();
            }
        }
        let e = verify(&drifted, &r).expect_err("second name is checked too");
        assert!(e.to_string().contains("vulkan.pastel.so"), "{e}");
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

    #[test]
    fn only_a_dri_render_node_can_be_bound() {
        assert!(valid_render_node("/dev/dri/renderD128"));
        assert!(valid_render_node("/dev/dri/renderD129"));
        for bad in [
            "",
            "/dev/dri/card0",
            "/dev/dri/renderD",
            "/dev/dri/renderD12a",
            "/dev/dri/renderD129/../card0",
            "/dev/dri/../sda",
            "/dev/sda",
            "dev/dri/renderD129",
            "/dev/dri/renderD12345",
        ] {
            assert!(!valid_render_node(bad), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn a_manifest_cannot_override_the_derived_properties() {
        assert!(valid_prop_key("ro.hardware.gralloc"));
        assert!(valid_prop_key("gralloc.gbm.device"));
        assert!(!valid_prop_key("ro.hardware.egl"));
        assert!(!valid_prop_key("ro.hardware.vulkan"));
        assert!(!valid_prop_key(""));
        assert!(!valid_prop_key("a b"));
        let dir = scratch("prop-key");
        let r = root(&dir);
        let mut p = install(&dir, "mesa-host", b"driver", "mesa");
        p.manifest
            .props
            .insert("ro.hardware.egl".into(), "evil".into());
        let base = dir.join("sysroot/gles/mesa-host");
        fs::write(
            base.join(MANIFEST),
            serde_json::to_string_pretty(&p.manifest).expect("json"),
        )
        .expect("manifest");
        let e = load(&r, "mesa-host").expect_err("must fail");
        assert!(e.to_string().contains("ro.hardware.egl"), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hand_edited_render_node_is_rejected_at_load_and_a_missing_one_is_named() {
        let dir = scratch("render-node");
        let r = root(&dir);
        let mut p = install(&dir, "mesa-host", b"driver", "mesa");
        let base = dir.join("sysroot/gles/mesa-host");
        p.manifest.render_node = Some("/dev/dri/card0".into());
        fs::write(
            base.join(MANIFEST),
            serde_json::to_string_pretty(&p.manifest).expect("json"),
        )
        .expect("manifest");
        let e = load(&r, "mesa-host").expect_err("card node must fail");
        assert!(e.to_string().contains("render_node"), "{e}");
        // A well-formed node that this host lacks is a structured error that
        // names the override, never a silent no-GPU launch.
        p.manifest.render_node = Some("/dev/dri/renderD9999".into());
        let missing = p.render_node().expect_err("absent node");
        assert!(
            missing.to_string().contains(RENDER_NODE_ENV),
            "{missing}"
        );
        // No render node declared (the ANGLE/SwiftShader providers): nothing.
        let q = install(&dir, "angle", b"driver", "angle");
        assert_eq!(q.render_node().expect("ok"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extra_props_and_render_node_round_trip_through_the_manifest() {
        let dir = scratch("props-rt");
        let r = root(&dir);
        let mut p = install(&dir, "mesa-host", b"driver", "mesa");
        p.manifest
            .props
            .insert("ro.hardware.gralloc".into(), "gbm".into());
        p.manifest.render_node = Some("/dev/dri/renderD129".into());
        let base = dir.join("sysroot/gles/mesa-host");
        fs::write(
            base.join(MANIFEST),
            serde_json::to_string_pretty(&p.manifest).expect("json"),
        )
        .expect("manifest");
        let q = load(&r, "mesa-host").expect("load");
        assert_eq!(q.manifest.props.get("ro.hardware.gralloc").map(String::as_str), Some("gbm"));
        assert_eq!(q.manifest.render_node.as_deref(), Some("/dev/dri/renderD129"));
        let _ = fs::remove_dir_all(&dir);
    }
}
