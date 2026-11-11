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
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// File name inside the writable data dir (`/data` in the sandbox).
pub const PROPS_FILE: &str = "acos.properties";

/// bionic `PROP_VALUE_MAX`.
pub const PROP_VALUE_MAX: usize = 92;

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
            ("ro.build.version.release", "14"),
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
            ("persist.device_config.runtime_native_boot.enable_uffd_gc", "false"),
            ("dalvik.vm.isa.x86_64.variant", "x86_64"),
            ("dalvik.vm.isa.x86_64.features", "default"),
            ("dalvik.vm.heaptargetutilization", "0.75"),
            ("dalvik.vm.heapstartsize", "8m"),
            ("dalvik.vm.heapgrowthlimit", "192m"),
            ("dalvik.vm.heapsize", "512m"),
            ("dalvik.vm.heapminfree", "512k"),
            ("dalvik.vm.heapmaxfree", "8m"),
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
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, self.to_string()).map_err(Error::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_servable() {
        let p = Props::android34();
        assert_eq!(p.get("ro.build.version.sdk"), Some("34"));
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
}
