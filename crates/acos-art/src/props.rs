//! The property table that `artshim.c` serves through bionic's
//! `__system_property_*` API.
//!
//! bionic 34 has no way to *create* a property area itself: `/dev/__properties__`
//! is prepared by `init` (a serialised `property_info` trie plus one
//! `u:object_r:<context>:s0` area per SELinux context), and every other process
//! inherits the mapping from it. ACOS runs no init, so `__system_property_get`
//! returns "" for everything — `dalvik.vm.*`, `ro.debuggable`, `ro.build.*`.
//!
//! Two options existed:
//!
//! * write the on-disk `property_info` + area files (the `libpropertyinfoparser`
//!   binary format: a `PropertyInfoArea` header, per-context `prop_area`s and a
//!   context-selection trie). Correct but ~300 lines of undocumented format
//!   work, and bionic still needs one area file per SELinux context;
//! * interpose the stable public C API (`__system_property_get/find/
//!   read_callback/serial/wait/foreach`) and serve a plain `key=value` table.
//!
//! We take the second (allowed by the task: "area file writer *or* an
//! interposer, whichever is simpler"): it is ~150 lines, has no format
//! archaeology, and it is observable — every value comes from a file the caller
//! can read. The interposer keeps bionic's exact in-memory `prop_info` layout,
//! so code that holds a `prop_info*` and reads `->value`/`__system_property_serial`
//! keeps working.
//!
//! File format: one `key=value` per line, `#` comments, values truncated to
//! `PROP_VALUE_MAX - 1` (91) bytes exactly like a real legacy property.
//! Device path: `/data/acos.properties`.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// File name inside the writable data dir (`/data` in the sandbox).
pub const PROPS_FILE: &str = "acos.properties";

/// bionic `PROP_VALUE_MAX`.
pub const PROP_VALUE_MAX: usize = 92;

// ---------------------------------------------------------------- heap seam
//
// ART's managed-heap limits are not a compile-time constant: `AndroidRuntime`
// reads six `dalvik.vm.heap*` properties and turns them into runtime flags
// (`.cache/aosp/base/core/jni/AndroidRuntime.cpp`):
//
// ```text
// parseRuntimeOption("dalvik.vm.heapstartsize",       buf, "-Xms", "4m");
// parseRuntimeOption("dalvik.vm.heapsize",            buf, "-Xmx", "16m");
// parseRuntimeOption("dalvik.vm.heapgrowthlimit",     buf, "-XX:HeapGrowthLimit=");
// parseRuntimeOption("dalvik.vm.heapminfree",         buf, "-XX:HeapMinFree=");
// parseRuntimeOption("dalvik.vm.heapmaxfree",         buf, "-XX:HeapMaxFree=");
// parseRuntimeOption("dalvik.vm.heaptargetutilization", buf, "-XX:HeapTargetUtilization=");
// ```
//
// On a device those come from `build.prop`/`default.prop`. ACOS serves its own
// table (this module), so the heap configuration was a constant in the source.
// The `ACOS_ART_HEAP*` env seam below makes it an experiment parameter **without
// ever falling back silently**: a malformed or inconsistent value is a
// structured `INVALID_HEAP_CONFIG` at launch, and the values that were actually
// written are recorded in the session's own metadata (`session-exit.json`,
// `live-session.json`, `launch --json`, the session report) so a benchmark row
// names the heap configuration that produced it.

/// `dalvik.vm.heapstartsize` (`-Xms`).
pub const ENV_HEAPSTARTSIZE: &str = "ACOS_ART_HEAPSTARTSIZE";
/// `dalvik.vm.heapgrowthlimit` (`-XX:HeapGrowthLimit=`).
pub const ENV_HEAPGROWTHLIMIT: &str = "ACOS_ART_HEAPGROWTHLIMIT";
/// `dalvik.vm.heapsize` (`-Xmx`).
pub const ENV_HEAPSIZE: &str = "ACOS_ART_HEAPSIZE";
/// `dalvik.vm.heapminfree` (`-XX:HeapMinFree=`).
pub const ENV_HEAPMINFREE: &str = "ACOS_ART_HEAPMINFREE";
/// `dalvik.vm.heapmaxfree` (`-XX:HeapMaxFree=`).
pub const ENV_HEAPMAXFREE: &str = "ACOS_ART_HEAPMAXFREE";
/// `dalvik.vm.heaptargetutilization` (`-XX:HeapTargetUtilization=`).
pub const ENV_HEAPTARGETUTILIZATION: &str = "ACOS_ART_HEAPTARGETUTILIZATION";

/// The stock API-34 `userdebug` heap values ACOS boots with (unchanged defaults).
pub const DEFAULT_HEAPSTARTSIZE: &str = "8m";
pub const DEFAULT_HEAPGROWTHLIMIT: &str = "192m";
pub const DEFAULT_HEAPSIZE: &str = "512m";
pub const DEFAULT_HEAPMINFREE: &str = "512k";
pub const DEFAULT_HEAPMAXFREE: &str = "8m";
pub const DEFAULT_HEAPTARGETUTILIZATION: &str = "0.75";

/// A refused heap-configuration value. `Display` states the variable, the value
/// and the expectation; there is no default to fall back to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeapConfigError {
    /// The environment variable (or property) at fault.
    pub var: &'static str,
    /// The value that was refused, verbatim.
    pub value: String,
    /// What was expected instead.
    pub problem: String,
}

impl std::fmt::Display for HeapConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}={:?}: {}", self.var, self.value, self.problem)
    }
}

impl std::error::Error for HeapConfigError {}

impl HeapConfigError {
    fn new(var: &'static str, value: &str, problem: impl Into<String>) -> Self {
        Self {
            var,
            value: value.to_string(),
            problem: problem.into(),
        }
    }
}

/// One `<int>[kmg]` size, as ART's own `-Xms`/`-XX:HeapGrowthLimit=` parsers see
/// it: decimal digits with an optional binary multiplier (`k`=2^10, `m`=2^20,
/// `g`=2^30), no bare whitespace, no uppercase, no fractional part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeapSize {
    /// The property text ART reads (`"8m"`, `"512k"`).
    pub text: String,
    /// The byte value that text denotes.
    pub bytes: u64,
}

impl HeapSize {
    /// Parse `<int>[kmg]`.
    pub fn parse(var: &'static str, text: &str) -> std::result::Result<Self, HeapConfigError> {
        let expected = "expected <int>[kmg]: decimal digits with an optional lowercase \
                        multiplier (k=1024, m=1024^2, g=1024^3), e.g. 512k, 8m, 2g";
        let (digits, mult) = match text.chars().last() {
            Some('k') => (&text[..text.len() - 1], 1u64 << 10),
            Some('m') => (&text[..text.len() - 1], 1u64 << 20),
            Some('g') => (&text[..text.len() - 1], 1u64 << 30),
            _ => (text, 1),
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(HeapConfigError::new(var, text, expected));
        }
        let n: u64 = digits
            .parse()
            .map_err(|_| HeapConfigError::new(var, text, expected))?;
        let bytes = n
            .checked_mul(mult)
            .ok_or_else(|| HeapConfigError::new(var, text, "size does not fit in 64 bits"))?;
        Ok(Self {
            text: text.to_string(),
            bytes,
        })
    }

    /// MiB, for the metadata record.
    pub fn mib(&self) -> f64 {
        self.bytes as f64 / (1024.0 * 1024.0)
    }
}

/// `dalvik.vm.heaptargetutilization`: a plain decimal strictly between 0 and 1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeapUtilization {
    /// The property text ART reads (`"0.75"`).
    pub text: String,
    /// The parsed fraction.
    pub value: f64,
}

impl HeapUtilization {
    /// Parse a decimal in (0,1).
    pub fn parse(var: &'static str, text: &str) -> std::result::Result<Self, HeapConfigError> {
        let expected = "expected a decimal strictly between 0 and 1, e.g. 0.75";
        if text.is_empty()
            || !text.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            || text.matches('.').count() > 1
            || text.starts_with('.')
            || text.ends_with('.')
        {
            return Err(HeapConfigError::new(var, text, expected));
        }
        let value: f64 = text
            .parse()
            .map_err(|_| HeapConfigError::new(var, text, expected))?;
        if !(value > 0.0 && value < 1.0) {
            return Err(HeapConfigError::new(
                var,
                text,
                format!("{expected}; {value} is outside (0,1)"),
            ));
        }
        Ok(Self {
            text: text.to_string(),
            value,
        })
    }
}

/// The six `dalvik.vm.heap*` values a launch will serve, after the
/// `ACOS_ART_HEAP*` overrides and validation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeapConfig {
    pub heapstartsize: HeapSize,
    pub heapgrowthlimit: HeapSize,
    pub heapsize: HeapSize,
    pub heapminfree: HeapSize,
    pub heapmaxfree: HeapSize,
    pub heaptargetutilization: HeapUtilization,
    /// Which `ACOS_ART_HEAP*` variables came from the environment (empty when
    /// the stock defaults are in force).
    pub overrides: Vec<String>,
}

impl HeapConfig {
    /// The stock API-34 values — exactly what [`Props::android34`] ships.
    pub fn defaults() -> Self {
        Self {
            heapstartsize: HeapSize::parse(ENV_HEAPSTARTSIZE, DEFAULT_HEAPSTARTSIZE)
                .expect("stock default"),
            heapgrowthlimit: HeapSize::parse(ENV_HEAPGROWTHLIMIT, DEFAULT_HEAPGROWTHLIMIT)
                .expect("stock default"),
            heapsize: HeapSize::parse(ENV_HEAPSIZE, DEFAULT_HEAPSIZE).expect("stock default"),
            heapminfree: HeapSize::parse(ENV_HEAPMINFREE, DEFAULT_HEAPMINFREE)
                .expect("stock default"),
            heapmaxfree: HeapSize::parse(ENV_HEAPMAXFREE, DEFAULT_HEAPMAXFREE)
                .expect("stock default"),
            heaptargetutilization: HeapUtilization::parse(
                ENV_HEAPTARGETUTILIZATION,
                DEFAULT_HEAPTARGETUTILIZATION,
            )
            .expect("stock default"),
            overrides: Vec::new(),
        }
    }

    /// Resolve the `ACOS_ART_HEAP*` environment over the stock defaults and
    /// validate. Returns the *first* bad value as a structured error; a present
    /// but empty/non-UTF-8 variable is an error, never `unset`.
    pub fn from_env() -> std::result::Result<Self, HeapConfigError> {
        fn pick(
            var: &'static str,
            default: &str,
            overrides: &mut Vec<String>,
        ) -> std::result::Result<String, HeapConfigError> {
            match std::env::var(var) {
                Ok(v) => {
                    overrides.push(var.to_string());
                    Ok(v)
                }
                Err(std::env::VarError::NotPresent) => Ok(default.to_string()),
                Err(std::env::VarError::NotUnicode(_)) => Err(HeapConfigError::new(
                    var,
                    "<non-UTF-8>",
                    "expected UTF-8 text; unset the variable to use the default",
                )),
            }
        }
        let mut overrides = Vec::new();
        let cfg = Self {
            heapstartsize: HeapSize::parse(
                ENV_HEAPSTARTSIZE,
                &pick(ENV_HEAPSTARTSIZE, DEFAULT_HEAPSTARTSIZE, &mut overrides)?,
            )?,
            heapgrowthlimit: HeapSize::parse(
                ENV_HEAPGROWTHLIMIT,
                &pick(ENV_HEAPGROWTHLIMIT, DEFAULT_HEAPGROWTHLIMIT, &mut overrides)?,
            )?,
            heapsize: HeapSize::parse(
                ENV_HEAPSIZE,
                &pick(ENV_HEAPSIZE, DEFAULT_HEAPSIZE, &mut overrides)?,
            )?,
            heapminfree: HeapSize::parse(
                ENV_HEAPMINFREE,
                &pick(ENV_HEAPMINFREE, DEFAULT_HEAPMINFREE, &mut overrides)?,
            )?,
            heapmaxfree: HeapSize::parse(
                ENV_HEAPMAXFREE,
                &pick(ENV_HEAPMAXFREE, DEFAULT_HEAPMAXFREE, &mut overrides)?,
            )?,
            heaptargetutilization: HeapUtilization::parse(
                ENV_HEAPTARGETUTILIZATION,
                &pick(
                    ENV_HEAPTARGETUTILIZATION,
                    DEFAULT_HEAPTARGETUTILIZATION,
                    &mut overrides,
                )?,
            )?,
            overrides,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// `heapstartsize <= heapgrowthlimit <= heapsize`. ART sized the growth cap
    /// from the growth limit and the hard cap from `-Xmx`; an inverted pair is a
    /// configuration bug, not something to quietly reorder.
    pub fn validate(&self) -> std::result::Result<(), HeapConfigError> {
        let (s, g, m) = (
            self.heapstartsize.bytes,
            self.heapgrowthlimit.bytes,
            self.heapsize.bytes,
        );
        let overridden = |var: &str| self.overrides.iter().any(|o| o == var);
        if s > g {
            // Name the variable the caller actually set; the other side may be a
            // stock default, and blaming it would send the reader to the wrong
            // place.
            let (var, value) = if overridden(ENV_HEAPSTARTSIZE) {
                (ENV_HEAPSTARTSIZE, &self.heapstartsize.text)
            } else {
                (ENV_HEAPGROWTHLIMIT, &self.heapgrowthlimit.text)
            };
            return Err(HeapConfigError::new(
                var,
                value,
                format!(
                    "heapstartsize {} must be <= heapgrowthlimit {}; the configured pair is \
                     inconsistent",
                    self.heapstartsize.text, self.heapgrowthlimit.text
                ),
            ));
        }
        if g > m {
            let (var, value) = if overridden(ENV_HEAPGROWTHLIMIT) {
                (ENV_HEAPGROWTHLIMIT, &self.heapgrowthlimit.text)
            } else {
                (ENV_HEAPSIZE, &self.heapsize.text)
            };
            return Err(HeapConfigError::new(
                var,
                value,
                format!(
                    "heapgrowthlimit {} must be <= heapsize {}; the configured pair is \
                     inconsistent",
                    self.heapgrowthlimit.text, self.heapsize.text
                ),
            ));
        }
        Ok(())
    }

    /// The `dalvik.vm.*` properties as ART's parser reads them.
    pub fn properties(&self) -> [(&'static str, &str); 6] {
        [
            ("dalvik.vm.heapstartsize", &self.heapstartsize.text),
            ("dalvik.vm.heapgrowthlimit", &self.heapgrowthlimit.text),
            ("dalvik.vm.heapsize", &self.heapsize.text),
            ("dalvik.vm.heapminfree", &self.heapminfree.text),
            ("dalvik.vm.heapmaxfree", &self.heapmaxfree.text),
            (
                "dalvik.vm.heaptargetutilization",
                &self.heaptargetutilization.text,
            ),
        ]
    }

    /// Write the six properties into a table.
    pub fn apply(&self, props: &mut Props) {
        for (k, v) in self.properties() {
            props.set(k, v);
        }
    }

    /// JSON for the session's own records: the effective property text, the
    /// byte values, and which variables were overridden.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "prop": {
                "dalvik.vm.heapstartsize": self.heapstartsize.text,
                "dalvik.vm.heapgrowthlimit": self.heapgrowthlimit.text,
                "dalvik.vm.heapsize": self.heapsize.text,
                "dalvik.vm.heapminfree": self.heapminfree.text,
                "dalvik.vm.heapmaxfree": self.heapmaxfree.text,
                "dalvik.vm.heaptargetutilization": self.heaptargetutilization.text,
            },
            "bytes": {
                "heapstartsize": self.heapstartsize.bytes,
                "heapgrowthlimit": self.heapgrowthlimit.bytes,
                "heapsize": self.heapsize.bytes,
                "heapminfree": self.heapminfree.bytes,
                "heapmaxfree": self.heapmaxfree.bytes,
            },
            "mib": {
                "heapstartsize": self.heapstartsize.mib(),
                "heapgrowthlimit": self.heapgrowthlimit.mib(),
                "heapsize": self.heapsize.mib(),
                "heapminfree": self.heapminfree.mib(),
                "heapmaxfree": self.heapmaxfree.mib(),
            },
            "heaptargetutilization": self.heaptargetutilization.value,
            "overridden": self.overrides,
            "source": "ACOS_ART_HEAP* overrides on the stock API-34 dalvik.vm.* table",
        })
    }
}

/// The heap configuration a run actually used, for session metadata. A run that
/// reached its metadata already validated the seam; an invalid value is still
/// reported as an error object rather than silently as the defaults.
pub fn effective_heap_json() -> serde_json::Value {
    match HeapConfig::from_env() {
        Ok(cfg) => cfg.to_json(),
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    }
}

#[derive(Debug, Clone, Default)]
pub struct Props {
    entries: BTreeMap<String, String>,
}

impl Props {
    /// The property set an API-34 `userdebug` device boots with, minus the
    /// vendor/device noise. These are the values ART and the framework read on
    /// the paths ACOS exercises (`dalvik.vm.*` sizes/flags, ABI, SDK level).
    pub fn android34() -> Self {
        let mut p = Props::default();
        for (k, v) in [
            ("ro.build.version.sdk", "34"),
            // libbinder's `defaultServiceManager()` (IServiceManager.cpp:152)
            // spins on this property before it ever asks for the context
            // object: `while (!WaitForProperty("servicemanager.ready", "true", 1s))`
            // logging ALOGE each second. ACOS has no servicemanager process,
            // but the *service manager* (the `IServiceManager` node answered by
            // `acos-binder-peer`/`ServiceHost`) is up before any guest starts, so
            // the value is true for this system: the wait ends and the retry
            // loop -- R1's log flood trigger -- cannot start.
            ("servicemanager.ready", "true"),
            ("ro.build.version.release", "14"),
            // libcutils `ashmem-dev.cpp::has_memfd_support()` is false without
            // these two properties (it logs "memfd: ro.vndk.version not defined"),
            // which sends every ashmem call down the `/dev/ashmem` ioctl path --
            // and the guest has no `/dev/ashmem`; every buffer ACOS hands out is a
            // memfd (the gralloc shim, `IGraphicsStats.requestBufferForProcess`,
            // `ParcelFd::memfd`). hwui's `ProfileDataContainer::switchStorageTo
            // Ashmem` then called `ashmem_get_size_region(memfd)` ->
            // `__ashmem_check_failure` -> `LOG_ALWAYS_FATAL("illegal fd=...")` and
            // the first thing in the app to start the RenderThread (the Gaten
            // lookup's screenshot copy) aborted (GATENE2E). `sys.use_memfd=true`
            // is the documented switch for "this device uses memfd-backed
            // ashmem" (the same file's own header); the kernel here has memfd +
            // F_SEAL_FUTURE_WRITE, so libcutils' self-detection passes.
            ("sys.use_memfd", "true"),
            ("ro.vndk.version", "34"),
            ("ro.build.version.codename", "REL"),
            ("ro.build.version.all_codenames", "REL"),
            ("ro.build.version.security_patch", "2024-01-01"),
            ("ro.build.id", "ACOS.ART.34"),
            ("ro.build.display.id", "acos-art-34"),
            ("ro.build.type", "userdebug"),
            ("ro.build.tags", "dev-keys"),
            ("ro.build.flavor", "acos_art-x86_64"),
            ("ro.debuggable", "1"),
            ("ro.secure", "0"),
            ("ro.adb.secure", "0"),
            ("ro.product.cpu.abi", "x86_64"),
            ("ro.product.cpu.abilist", "x86_64"),
            ("ro.product.cpu.abilist64", "x86_64"),
            ("ro.product.cpu.abilist32", ""),
            ("ro.product.model", "ACOS ART"),
            ("ro.product.brand", "acos"),
            ("ro.product.name", "acos_art"),
            ("ro.product.device", "acos"),
            ("ro.product.manufacturer", "ACOS"),
            ("ro.hardware", "acos"),
            ("ro.kernel.qemu", "0"),
            ("ro.zygote", "zygote64"),
            ("ro.dalvik.vm.native.bridge", "0"),
            ("ro.config.low_ram", "false"),
            ("ro.sf.lcd_density", "420"),
            ("persist.sys.dalvik.vm.lib.2", "libart.so"),
            (
                "persist.device_config.runtime_native_boot.enable_uffd_gc",
                "false",
            ),
            ("dalvik.vm.isa.x86_64.variant", "x86_64"),
            ("dalvik.vm.isa.x86_64.features", "default"),
            (
                "dalvik.vm.heaptargetutilization",
                DEFAULT_HEAPTARGETUTILIZATION,
            ),
            ("dalvik.vm.heapstartsize", DEFAULT_HEAPSTARTSIZE),
            ("dalvik.vm.heapgrowthlimit", DEFAULT_HEAPGROWTHLIMIT),
            ("dalvik.vm.heapsize", DEFAULT_HEAPSIZE),
            ("dalvik.vm.heapminfree", DEFAULT_HEAPMINFREE),
            ("dalvik.vm.heapmaxfree", DEFAULT_HEAPMAXFREE),
            ("dalvik.vm.dex2oat-filter", "verify"),
            ("dalvik.vm.dex2oat-image-filter", "verify"),
            ("dalvik.vm.usejit", "true"),
            ("dalvik.vm.usejitprofiles", "true"),
            ("dalvik.vm.jitthreshold", "2048"),
            ("dalvik.vm.jitinitialsize", "64k"),
            ("dalvik.vm.jitmaxsize", "64m"),
            ("dalvik.vm.stack-trace-file", "/data/anr/traces.txt"),
            ("dalvik.vm.checkjni", "false"),
            ("dalvik.vm.dexopt.secondary", "true"),
            ("acos.art", "1"),
            ("acos.art.runtime", "dalvikvm64"),
        ] {
            p.entries.insert(k.to_string(), v.to_string());
        }
        p
    }

    /// The table a **launch** serves: the stock table with the validated
    /// `ACOS_ART_HEAP*` overrides applied. An invalid value is
    /// `INVALID_HEAP_CONFIG`, never a silent default.
    pub fn for_launch() -> Result<Self> {
        let heap = HeapConfig::from_env().map_err(|e| Error::InvalidHeapConfig(e.to_string()))?;
        let mut p = Self::android34();
        heap.apply(&mut p);
        // GLSPIKE: the image's own EGL loader appends `ro.hardware.egl` to the
        // driver name it looks for in `/vendor/lib64/egl` (`Loader.cpp`,
        // `HAL_SUBNAME_KEY_PROPERTIES`). Default: the property stays unset, so
        // libEGL keeps its own "couldn't find an OpenGL ES implementation"
        // fatal — exactly today's guest. With `ACOS_GUEST_GLES=<provider>` the
        // run opted into a staged provider and the loader can find it.
        //
        // The value is the manifest **tag** (`libEGL_<tag>.so`), not the
        // provider directory: GLSPIKE2's `angle-swiftshader` provider stages
        // `libEGL_angle.so`, so a run that served the directory name aborted
        // inside libEGL one step before ANGLE.
        //
        // GLSPIKE2: a provider that also stages a Vulkan ICD (ANGLE needs one)
        // declares `ro.hardware.vulkan`, and the image's own `libvulkan.so`
        // opens `/vendor/lib64/hw/vulkan.<tag>.so` from it (`hwvulkan module
        // open`, `vulkan.`, measured in the sysroot's libvulkan). Absent when
        // the provider carries no ICD: the property stays unset, exactly as in
        // the image today.
        if let Some((tag, vulkan)) = crate::gles::selected_tags()? {
            p.set("ro.hardware.egl", &tag);
            // The probe fixtures read this to decide whether to take the GL
            // path at all; an app on a device never needs it (it just draws).
            p.set("acos.guest.gles", &tag);
            if let Some(vulkan) = vulkan {
                p.set("ro.hardware.vulkan", &vulkan);
                p.set("acos.guest.gles.vulkan", &vulkan);
            }
        }
        Ok(p)
    }

    pub fn set(&mut self, key: &str, value: &str) -> &mut Self {
        self.entries.insert(key.to_string(), value.to_string());
        self
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn parse(text: &str) -> Self {
        let mut p = Props::default();
        for line in text.lines() {
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                p.entries.insert(k.trim().to_string(), v.to_string());
            }
        }
        p
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)?;
        Ok(Self::parse(&text))
    }

    /// Serialise the table. Values longer than 91 bytes are truncated, which is
    /// what a real `PROP_VALUE_MAX` property read would return.
    pub fn to_string(&self) -> String {
        let mut out = String::from(
            "# acos-art property table (served by libartshim.so __system_property_*)\n\
             # format: key=value, one per line, '#' starts a comment\n",
        );
        for (k, v) in &self.entries {
            if k.is_empty() || k.len() >= PROP_VALUE_MAX || k.contains(['=', '\n']) {
                continue;
            }
            let v: String = v.chars().take(PROP_VALUE_MAX - 1).collect();
            out.push_str(&format!("{k}={v}\n"));
        }
        out
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        // H17: this table is read by every guest of a concurrent cohort, so it
        // is published atomically and only when it changed (see
        // [`crate::publish_atomic`]).
        crate::publish_atomic(path, self.to_string().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `ACOS_ART_HEAP*` variable, so a test can clear all six and know the
    /// defaults it observes are really the stock table.
    const ALL_HEAP_ENV: [&str; 6] = [
        ENV_HEAPSTARTSIZE,
        ENV_HEAPGROWTHLIMIT,
        ENV_HEAPSIZE,
        ENV_HEAPMINFREE,
        ENV_HEAPMAXFREE,
        ENV_HEAPTARGETUTILIZATION,
    ];

    /// The process environment is global; the tests that mutate it take the
    /// crate-wide lock and run one at a time (and restore what they found).
    use crate::ENV_LOCK;

    fn with_env(vars: &[(&'static str, &str)], f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(&str, Option<String>)> = ALL_HEAP_ENV
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();
        for k in ALL_HEAP_ENV {
            std::env::remove_var(k);
        }
        for (k, v) in vars {
            std::env::set_var(k, v);
        }
        f();
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    /// GLSHIP: software GL is ON by default when the provider is installed, so
    /// a test that compares the **whole** launch table must pin that seam.
    /// The caller holds `ENV_LOCK` (directly or through [`with_env`]).
    fn with_gles_off<T>(f: impl FnOnce() -> T) -> T {
        let saved = std::env::var("ACOS_GUEST_GLES").ok();
        std::env::set_var("ACOS_GUEST_GLES", "off");
        let out = f();
        match saved {
            Some(v) => std::env::set_var("ACOS_GUEST_GLES", v),
            None => std::env::remove_var("ACOS_GUEST_GLES"),
        }
        out
    }

    #[test]
    fn defaults_are_servable() {
        let p = Props::android34();
        assert_eq!(p.get("ro.build.version.sdk"), Some("34"));
        // GATENE2E: libcutils only uses memfd-backed ashmem with these two set.
        assert_eq!(p.get("sys.use_memfd"), Some("true"));
        assert_eq!(p.get("ro.vndk.version"), Some("34"));
        assert_eq!(p.get("dalvik.vm.heapgrowthlimit"), Some("192m"));
        let text = p.to_string();
        assert!(text.contains("ro.debuggable=1\n"));
        let round = Props::parse(&text);
        assert_eq!(round.get("ro.build.version.release"), Some("14"));
    }

    #[test]
    fn long_values_are_truncated() {
        let mut p = Props::default();
        p.set("k", &"x".repeat(200));
        let text = p.to_string();
        assert_eq!(text.lines().last().unwrap().len(), 2 + PROP_VALUE_MAX - 1);
    }

    // ---- heap configuration seam ------------------------------------------

    /// Defaults stay exactly as they are today: with no `ACOS_ART_HEAP*` set,
    /// the table is byte-identical to the stock one and no override is claimed.
    #[test]
    fn heap_defaults_unchanged() {
        with_env(&[], || with_gles_off(|| {
            let cfg = HeapConfig::from_env().expect("defaults must validate");
            assert_eq!(cfg, HeapConfig::defaults());
            assert!(cfg.overrides.is_empty());
            assert_eq!(cfg.heapstartsize.text, "8m");
            assert_eq!(cfg.heapgrowthlimit.text, "192m");
            assert_eq!(cfg.heapsize.text, "512m");
            assert_eq!(cfg.heapminfree.text, "512k");
            assert_eq!(cfg.heapmaxfree.text, "8m");
            assert_eq!(cfg.heaptargetutilization.text, "0.75");
            assert_eq!(cfg.heapstartsize.bytes, 8 << 20);
            assert_eq!(cfg.heapgrowthlimit.bytes, 192 << 20);
            assert_eq!(cfg.heapsize.bytes, 512 << 20);

            // The launch table is the stock table, property for property.
            let launch = Props::for_launch().expect("defaults are valid");
            assert_eq!(launch.to_string(), Props::android34().to_string());
            for (k, v) in cfg.properties() {
                assert_eq!(launch.get(k), Some(v));
            }
        }));
    }

    /// Each override moves exactly its own property and is recorded as an
    /// override.
    #[test]
    fn heap_each_override_applied() {
        let cases: [(&'static str, &str, &str); 6] = [
            (ENV_HEAPSTARTSIZE, "4m", "dalvik.vm.heapstartsize"),
            (ENV_HEAPGROWTHLIMIT, "64m", "dalvik.vm.heapgrowthlimit"),
            (ENV_HEAPSIZE, "256m", "dalvik.vm.heapsize"),
            (ENV_HEAPMINFREE, "1m", "dalvik.vm.heapminfree"),
            (ENV_HEAPMAXFREE, "16m", "dalvik.vm.heapmaxfree"),
            (
                ENV_HEAPTARGETUTILIZATION,
                "0.5",
                "dalvik.vm.heaptargetutilization",
            ),
        ];
        for (var, value, prop) in cases {
            with_env(&[(var, value)], || {
                let props = Props::for_launch().expect("a single override must validate");
                assert_eq!(props.get(prop), Some(value), "{var} did not reach {prop}");
                let cfg = HeapConfig::from_env().expect("valid");
                assert_eq!(cfg.overrides, vec![var.to_string()]);
                // Everything else is still the stock value.
                for (k, v) in HeapConfig::defaults().properties() {
                    if k != prop {
                        assert_eq!(props.get(k), Some(v), "{var} changed {k}");
                    }
                }
            });
        }
    }

    /// Both overrides of the guarded W48 run, together, reach the table.
    #[test]
    fn heap_start_and_growth_override_together() {
        with_env(
            &[(ENV_HEAPSTARTSIZE, "4m"), (ENV_HEAPGROWTHLIMIT, "64m")],
            || {
                let cfg = HeapConfig::from_env().expect("4m <= 64m <= 512m");
                assert_eq!(cfg.heapstartsize.bytes, 4 << 20);
                assert_eq!(cfg.heapgrowthlimit.bytes, 64 << 20);
                assert_eq!(cfg.heapsize.bytes, 512 << 20);
                assert_eq!(cfg.overrides.len(), 2);
                let json = cfg.to_json();
                assert_eq!(json["prop"]["dalvik.vm.heapgrowthlimit"], "64m");
                assert_eq!(json["bytes"]["heapgrowthlimit"], 64u64 << 20);
                let props = Props::for_launch().expect("valid");
                assert_eq!(props.get("dalvik.vm.heapstartsize"), Some("4m"));
                assert_eq!(props.get("dalvik.vm.heapgrowthlimit"), Some("64m"));
            },
        );
    }

    /// A malformed value is refused, never replaced by the default. Both the
    /// typed error and the launch path carry `INVALID_HEAP_CONFIG`.
    #[test]
    fn heap_invalid_format_rejected() {
        let bad = [
            "8x",
            "8M",
            "8 m",
            "",
            "m",
            "8.5m",
            "-8m",
            "+8m",
            "0x10m",
            "99999999999999999999m",
            "8mm",
            "8g ",
        ];
        for value in bad {
            let err = HeapSize::parse(ENV_HEAPSIZE, value)
                .expect_err(&format!("{value:?} must be refused"));
            assert_eq!(err.var, ENV_HEAPSIZE);
            assert_eq!(err.value, value);
            with_env(&[(ENV_HEAPSIZE, value)], || {
                let launch = Props::for_launch().expect_err("invalid must not launch");
                let msg = launch.to_string();
                assert!(
                    msg.starts_with("INVALID_HEAP_CONFIG:"),
                    "unexpected error: {msg}"
                );
                assert!(
                    msg.contains(ENV_HEAPSIZE),
                    "error must name the variable: {msg}"
                );
            });
        }
        for value in ["0", "1", "1.5", ".5", "0.0", "abc", "0.75.1", ""] {
            HeapUtilization::parse(ENV_HEAPTARGETUTILIZATION, value)
                .expect_err(&format!("utilization {value:?} must be refused"));
        }
        for ok in ["0.75", "0.5", "0.999"] {
            HeapUtilization::parse(ENV_HEAPTARGETUTILIZATION, ok).expect("valid utilization");
        }
        for ok in ["512k", "8m", "2g", "4096"] {
            HeapSize::parse(ENV_HEAPSIZE, ok).expect("valid size");
        }
    }

    /// `heapstartsize <= heapgrowthlimit <= heapsize`; an inverted pair is a
    /// structured error, not a silent reorder.
    #[test]
    fn heap_inconsistent_ordering_rejected() {
        with_env(&[(ENV_HEAPSTARTSIZE, "256m")], || {
            let err = HeapConfig::from_env().expect_err("256m start > 192m growth limit");
            assert_eq!(err.var, ENV_HEAPSTARTSIZE);
            assert!(err.to_string().contains("heapgrowthlimit"), "{err}");
            let msg = Props::for_launch()
                .expect_err("must not launch")
                .to_string();
            assert!(msg.starts_with("INVALID_HEAP_CONFIG:"), "{msg}");
        });
        with_env(&[(ENV_HEAPGROWTHLIMIT, "1g")], || {
            let err = HeapConfig::from_env().expect_err("growth limit > heap size");
            assert_eq!(err.var, ENV_HEAPGROWTHLIMIT);
            let msg = Props::for_launch()
                .expect_err("must not launch")
                .to_string();
            assert!(msg.starts_with("INVALID_HEAP_CONFIG:"), "{msg}");
        });
        with_env(
            &[(ENV_HEAPSIZE, "16m"), (ENV_HEAPGROWTHLIMIT, "32m")],
            || {
                HeapConfig::from_env().expect_err("32m growth limit > 16m heap");
            },
        );
        // Equal is allowed (start == growthlimit == heapsize).
        with_env(
            &[
                (ENV_HEAPSTARTSIZE, "64m"),
                (ENV_HEAPGROWTHLIMIT, "64m"),
                (ENV_HEAPSIZE, "64m"),
            ],
            || {
                HeapConfig::from_env().expect("equal values are consistent");
            },
        );
    }

    /// The session metadata records the effective values, not the defaults.
    #[test]
    fn heap_metadata_reports_effective_values() {
        with_env(&[(ENV_HEAPGROWTHLIMIT, "64m")], || {
            let doc = effective_heap_json();
            assert_eq!(doc["prop"]["dalvik.vm.heapgrowthlimit"], "64m");
            assert_eq!(doc["prop"]["dalvik.vm.heapstartsize"], "8m");
            assert_eq!(doc["overridden"][0], ENV_HEAPGROWTHLIMIT);
            assert_eq!(doc["bytes"]["heapgrowthlimit"], 64u64 << 20);
        });
        with_env(&[(ENV_HEAPSTARTSIZE, "nope")], || {
            let doc = effective_heap_json();
            assert!(doc["error"].as_str().unwrap().contains(ENV_HEAPSTARTSIZE));
        });
    }

    /// GLSPIKE2: `ro.hardware.egl` is the provider's manifest **tag**, the value
    /// the image's loader appends to `libEGL_`, never the `ACOS_GUEST_GLES`
    /// directory name. A run whose property was the directory name aborted
    /// inside libEGL with its own `couldn't find an OpenGL ES implementation`
    /// fatal (measured: results/handoff/GLSPIKE2), one step before the driver.
    /// The ICD the provider stages comes with it as `ro.hardware.vulkan`.
    ///
    /// GLSHIP: the provider is the **default** when it is installed, so the
    /// matrix is (nothing installed | installed) x (no flag | off | named).
    #[test]
    fn the_gles_properties_are_the_manifest_tag_and_its_vulkan_icd() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("acos-props-gles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let gles = dir.join("gles");
        std::fs::create_dir_all(&gles).expect("dirs");
        let saved: Vec<(&str, Option<String>)> = ["ACOS_GUEST_GLES", "ACOS_GUEST_GLES_DIR"]
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();
        let result: Props = (|| {
            // (1) nothing installed, no flag: today's guest, properties unset.
            std::env::remove_var("ACOS_GUEST_GLES");
            std::env::set_var("ACOS_GUEST_GLES_DIR", &gles);
            let none = Props::for_launch().expect("defaults");
            assert_eq!(none.get("ro.hardware.egl"), None, "not installed: unset");
            assert_eq!(none.get("ro.hardware.vulkan"), None);

            // (2) installed: the default provider is served with no flag.
            let provider = gles.join("angle-swiftshader");
            std::fs::create_dir_all(provider.join("lib64/egl")).expect("dirs");
            std::fs::create_dir_all(provider.join("lib64/hw")).expect("dirs");
            std::fs::write(provider.join("lib64/egl/libEGL_angle.so"), b"driver").expect("write");
            std::fs::write(provider.join("lib64/hw/vulkan.pastel.so"), b"icd").expect("write");
            std::fs::write(provider.join("lib64/vulkan.pastel.so"), b"icd").expect("write");
            std::fs::write(
                provider.join("manifest.json"),
                r#"{"tag":"angle","vulkan_tag":"pastel","provider":"x","source":"y",
                    "version":"0","licence":"z","files":[
                    {"path":"lib64/egl/libEGL_angle.so","sha256":"00","size":6},
                    {"path":"lib64/hw/vulkan.pastel.so","sha256":"00","size":3},
                    {"path":"lib64/vulkan.pastel.so","sha256":"00","size":3}]}"#,
            )
            .expect("manifest");
            let default_on = Props::for_launch().expect("default provider");
            assert_eq!(default_on.get("ro.hardware.egl"), Some("angle"));
            assert_eq!(default_on.get("ro.hardware.vulkan"), Some("pastel"));

            // (3) `off` wins over an installed provider.
            std::env::set_var("ACOS_GUEST_GLES", "off");
            let off = Props::for_launch().expect("off");
            assert_eq!(off.get("ro.hardware.egl"), None, "ACOS_GUEST_GLES=off: unset");
            assert_eq!(off.get("ro.hardware.vulkan"), None);

            // (4) an explicit name serves the same provider.
            std::env::set_var("ACOS_GUEST_GLES", "angle-swiftshader");
            Props::for_launch().expect("a staged provider is valid")
        })();
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        let on = result;
        assert_eq!(
            on.get("ro.hardware.egl"),
            Some("angle"),
            "the directory name is a label; the loader appends the tag"
        );
        assert_eq!(on.get("acos.guest.gles"), Some("angle"));
        assert_eq!(on.get("ro.hardware.vulkan"), Some("pastel"));
        assert_eq!(on.get("acos.guest.gles.vulkan"), Some("pastel"));
    }
}
