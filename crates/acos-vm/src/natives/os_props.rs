//! `android.os.SystemProperties` — the process property table.
//!
//! `android.os.Build` and every `android.sysprop.*` class read the build
//! identity through `SystemProperties`, so the table is seeded from the same
//! file the reference image ships: `.cache/boot/android-34/build.prop`
//! (extracted by `tools/extract-bootclasspath.sh`; size and SHA-256 are in
//! `.cache/boot/android-34/manifest.json`).
//!
//! Two properties `Build` reads are **computed by init, not stored** in that
//! file, because it is the *system partition's* `build.prop`:
//!
//! * `ro.build.fingerprint` ← `ro.system.build.fingerprint`
//! * `ro.product.cpu.abilist{,32,64}` ← `ro.system.product.cpu.abilist{,32,64}`
//!
//! ACOS derives them with the same rule instead of hardcoding a fingerprint, so
//! `Build.FINGERPRINT` / `Build.SUPPORTED_ABIS` match the image. The legacy
//! `ro.product.{brand,device,manufacturer,model,name}` aliases are derived from
//! their `ro.product.system.*` counterparts for the same reason; a full device
//! image would take those from `vendor`/`odm`, which the cache does not contain.
//!
//! Missing-key semantics follow ART exactly:
//!
//! * `native_get(String)` → `""` (that is what `SystemProperties.get(key)`
//!   returns on a device for an unset key),
//! * `native_get(String,String)`, `native_get_int/long/boolean` and the
//!   `$Handle` accessors → the caller's default,
//! * `native_find(String)` → `0`, so `SystemProperties.find()` returns `null`,
//! * `ro.*` is read-only: `native_set` on a `ro.*` key is ignored, like init.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::NULL;
use crate::vm::Vm;

/// Where one value came from. Reported so tests (and later `acos verify`) can
/// distinguish "read from the image" from "computed" from a fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    /// Parsed from the extracted API-34 `build.prop`.
    BuildProp,
    /// Computed from another property with init's rule (see module docs).
    Derived,
    /// Compiled-in capture from `research/ambient-state/sysprops.tsv` /
    /// `fixtures/reference/**/device-config.json`, used only when no
    /// `build.prop` is on disk.
    ArtCapture,
    /// Written at runtime by `SystemProperties.set` (never a `ro.*` key).
    Runtime,
}

/// Which seed backed the process property table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeedSource {
    /// Parsed from this `build.prop`.
    BuildProp(PathBuf),
    /// `build.prop` was absent; the compiled-in ART capture was used.
    Embedded,
}

/// One property definition.
#[derive(Clone, Debug)]
pub struct PropEntry {
    pub key: String,
    pub value: String,
    pub provenance: Provenance,
    /// Stable `SystemProperties$Handle` id; `0` is never a valid handle.
    id: u32,
}

/// The process property table (`__system_property_area__` equivalent).
#[derive(Clone, Debug)]
pub struct PropTable {
    entries: Vec<PropEntry>,
    index: HashMap<String, usize>,
    by_id: HashMap<u32, usize>,
    next_id: u32,
    source: SeedSource,
}

impl Default for PropTable {
    fn default() -> PropTable {
        PropTable {
            entries: Vec::new(),
            index: HashMap::new(),
            by_id: HashMap::new(),
            next_id: 1,
            source: SeedSource::Embedded,
        }
    }
}

impl PropTable {
    /// Parse `build.prop` text. Comment lines (`#`) and blank lines are
    /// skipped, `key=value` is split at the first `=`, a repeated key keeps its
    /// position and takes the later value (init applies later assignments last).
    pub fn from_build_prop(text: &str, source: SeedSource) -> PropTable {
        let mut table = PropTable {
            source,
            ..PropTable::default()
        };
        for raw in text.lines() {
            let line = raw.trim_end_matches('\r').trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            table.insert(key, value.trim(), Provenance::BuildProp);
        }
        table
    }

    fn insert(&mut self, key: &str, value: &str, provenance: Provenance) {
        if let Some(&i) = self.index.get(key) {
            self.entries[i].value = value.to_string();
            self.entries[i].provenance = provenance;
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let i = self.entries.len();
        self.entries.push(PropEntry {
            key: key.to_string(),
            value: value.to_string(),
            provenance,
            id,
        });
        self.index.insert(key.to_string(), i);
        self.by_id.insert(id, i);
    }

    fn remove(&mut self, key: &str) -> bool {
        let Some(i) = self.index.remove(key) else {
            return false;
        };
        let id = self.entries[i].id;
        self.by_id.remove(&id);
        self.entries.remove(i);
        // Re-index the tail after the removal.
        for (j, e) in self.entries.iter().enumerate().skip(i) {
            self.index.insert(e.key.clone(), j);
        }
        true
    }

    /// Apply init's derivations to a table parsed from `build.prop`.
    fn derive(&mut self) {
        self.derive_one("ro.build.fingerprint", "ro.system.build.fingerprint");
        for name in ["abilist", "abilist32", "abilist64"] {
            let src = format!("ro.system.product.cpu.{name}");
            let dst = format!("ro.product.cpu.{name}");
            self.derive_one(&dst, &src);
        }
        // init's legacy `ro.product.*` aliases for a system-only image.
        for name in ["brand", "device", "manufacturer", "model", "name"] {
            let src = format!("ro.product.system.{name}");
            let dst = format!("ro.product.{name}");
            self.derive_one(&dst, &src);
        }
    }

    fn derive_one(&mut self, dst: &str, src: &str) {
        if self.index.contains_key(dst) {
            return;
        }
        let Some(value) = self.index.get(src).map(|&i| self.entries[i].value.clone()) else {
            return;
        };
        self.insert(dst, &value, Provenance::Derived);
    }

    /// Value for `key`, or `None` when the property is unset.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.index.get(key).map(|&i| self.entries[i].value.as_str())
    }

    /// Provenance of `key`, if set.
    pub fn provenance(&self, key: &str) -> Option<Provenance> {
        self.index.get(key).map(|&i| self.entries[i].provenance)
    }

    pub fn source(&self) -> &SeedSource {
        &self.source
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[PropEntry] {
        &self.entries
    }

    /// Write a property. Returns `false` when the key is read-only (`ro.*`) or
    /// the key is empty, mirroring init's refusal to change `ro.*` at runtime.
    pub fn set(&mut self, key: &str, value: &str) -> bool {
        if key.is_empty() || key.starts_with("ro.") {
            return false;
        }
        self.insert(key, value, Provenance::Runtime);
        true
    }

    /// Delete a runtime property (`SystemProperties.set(key, null)`).
    pub fn unset(&mut self, key: &str) -> bool {
        if key.starts_with("ro.") {
            return false;
        }
        self.remove(key)
    }

    /// Stable handle id for a set key (`SystemProperties.native_find`).
    pub fn find_handle(&self, key: &str) -> u64 {
        self.index
            .get(key)
            .map(|&i| self.entries[i].id as u64)
            .unwrap_or(0)
    }

    /// Key behind a handle id, or `None` if it was deleted after `find`.
    pub fn key_for_handle(&self, handle: u64) -> Option<&str> {
        let i = *self.by_id.get(&(handle as u32))?;
        Some(self.entries[i].key.as_str())
    }

    /// Value behind a handle id (`SystemProperties.native_get(J)`).
    pub fn value_for_handle(&self, handle: u64) -> Option<&str> {
        let i = *self.by_id.get(&(handle as u32))?;
        Some(self.entries[i].value.as_str())
    }
}

/// The compiled-in fallback table.
///
/// Used only when no `build.prop` is on disk (CI without the extracted image).
/// Values are the ART-captured ones: `research/ambient-state/sysprops.tsv`
/// (`value_to_supply`, `known` rows marked "extracted from the Android 14
/// image") plus `fixtures/reference/**/device-config.json` for the fingerprint.
const EMBEDDED_PROPS: &[(&str, &str)] = &[
    ("ro.build.id", "UE1A.230829.050"),
    ("ro.build.version.incremental", "12077443"),
    ("ro.build.version.sdk", "34"),
    ("ro.build.version.preview_sdk", "0"),
    ("ro.build.version.preview_sdk_fingerprint", "REL"),
    ("ro.build.version.codename", "REL"),
    ("ro.build.version.release", "14"),
    ("ro.build.version.release_or_codename", "14"),
    ("ro.build.version.release_or_preview_display", "14"),
    ("ro.build.version.security_patch", "2023-09-05"),
    ("ro.build.version.min_supported_target_sdk", "28"),
    ("ro.build.type", "userdebug"),
    ("ro.build.tags", "dev-keys"),
    ("ro.build.user", "android-build"),
    (
        "ro.build.fingerprint",
        "google/sdk_gphone64_x86_64/emu64xa:14/UE1A.230829.050/12077443:userdebug/dev-keys",
    ),
    ("ro.product.cpu.abi", "x86_64"),
    ("ro.product.cpu.abilist", "x86_64,arm64-v8a"),
    ("ro.product.cpu.abilist32", ""),
    ("ro.product.cpu.abilist64", "x86_64,arm64-v8a"),
    ("ro.product.locale", "en-US"),
    ("ro.product.model", "sdk_gphone64_x86_64"),
    ("ro.product.device", "emu64xa"),
    ("ro.product.brand", "google"),
    ("ro.debuggable", "1"),
    ("ro.secure", "1"),
];

fn build_table() -> PropTable {
    if let Some(path) = crate::vm::default_boot_dir(None).map(|d| d.join("build.prop")) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let mut table = PropTable::from_build_prop(&text, SeedSource::BuildProp(path));
            table.derive();
            if !table.is_empty() {
                return table;
            }
        }
    }
    let mut table = PropTable {
        source: SeedSource::Embedded,
        ..PropTable::default()
    };
    for (key, value) in EMBEDDED_PROPS {
        table.insert(key, value, Provenance::ArtCapture);
    }
    table
}

/// The process-wide property table, seeded on first use.
fn global() -> &'static Mutex<PropTable> {
    static TABLE: OnceLock<Mutex<PropTable>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(build_table()))
}

fn lock() -> MutexGuard<'static, PropTable> {
    global().lock().unwrap_or_else(|p| p.into_inner())
}

/// Where the seeded table came from (tests and the boot report).
pub fn seed_source() -> SeedSource {
    lock().source().clone()
}

/// Value of a property, or `None` when unset.
pub fn get(key: &str) -> Option<String> {
    lock().get(key).map(str::to_string)
}

/// Provenance of a property, when set.
pub fn provenance(key: &str) -> Option<Provenance> {
    lock().provenance(key)
}

/// Number of properties in the table.
pub fn len() -> usize {
    lock().len()
}

/// Write a runtime property (`SystemProperties.set`); `ro.*` is refused.
pub fn set(key: &str, value: &str) -> bool {
    lock().set(key, value)
}

/// Delete a runtime property (`SystemProperties.set(key, null)`).
pub fn unset(key: &str) -> bool {
    lock().unset(key)
}

/// ART's decimal `native_get_int` / `native_get_long` parsing: a strict
/// base-10 integer with an optional sign, otherwise the caller's default.
pub fn parse_i64(raw: Option<&str>, def: i64) -> i64 {
    let Some(s) = raw else {
        return def;
    };
    let s = s.trim();
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return def;
    }
    s.parse::<i64>().unwrap_or(def)
}

/// `native_get_int`: parse as long, then require the value to fit in an `int`.
pub fn parse_i32(raw: Option<&str>, def: i32) -> i32 {
    let v = parse_i64(raw, def as i64);
    i32::try_from(v).unwrap_or(def)
}

/// `native_get_boolean`: the documented token table, otherwise the default.
/// (`y/yes/1/true/on` → true, `n/no/0/false/off` → false, case sensitive.)
pub fn parse_bool(raw: Option<&str>, def: bool) -> bool {
    match raw {
        Some("y" | "yes" | "1" | "true" | "on") => true,
        Some("n" | "no" | "0" | "false" | "off") => false,
        _ => def,
    }
}

pub(crate) fn install(vm: &mut Vm) {
    let c = "Landroid/os/SystemProperties;";
    // `String`-keyed entry points.
    vm.register_native(
        c,
        "native_get",
        "(Ljava/lang/String;)Ljava/lang/String;",
        native_get_string,
    );
    vm.register_native(
        c,
        "native_get",
        "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
        native_get_string_def,
    );
    vm.register_native(
        c,
        "native_get_int",
        "(Ljava/lang/String;I)I",
        native_get_int_string,
    );
    vm.register_native(
        c,
        "native_get_long",
        "(Ljava/lang/String;J)J",
        native_get_long_string,
    );
    vm.register_native(
        c,
        "native_get_boolean",
        "(Ljava/lang/String;Z)Z",
        native_get_boolean_string,
    );
    vm.register_native(c, "native_find", "(Ljava/lang/String;)J", native_find);
    vm.register_native(
        c,
        "native_set",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        native_set,
    );
    // `SystemProperties$Handle`-keyed entry points (`find()` → `getInt()` …).
    vm.register_native(c, "native_get", "(J)Ljava/lang/String;", native_get_handle);
    vm.register_native(c, "native_get_int", "(JI)I", native_get_int_handle);
    vm.register_native(c, "native_get_long", "(JJ)J", native_get_long_handle);
    vm.register_native(c, "native_get_boolean", "(JZ)Z", native_get_boolean_handle);
    vm.register_native(
        c,
        "native_add_change_callback",
        "()V",
        native_add_change_callback,
    );
    vm.register_native(
        c,
        "native_report_sysprop_change",
        "()V",
        native_report_sysprop_change,
    );
}

/// Read a `String` argument, throwing `NullPointerException` for null.
fn require_string(vm: &mut Vm, ctx: NativeCtx, n: usize) -> Result<String, VmError> {
    let h = vm.arg_ref(ctx, n);
    if h == NULL {
        return Err(vm.throw_npe("key"));
    }
    Ok(vm.string_to_rust(h).unwrap_or_default())
}

fn native_get_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let key = require_string(vm, ctx, 0)?;
    let h = match get(&key) {
        Some(v) => vm.new_string(&v)?,
        // ART returns the empty string (not null) for an unset key here.
        None => vm.intern_str("")?,
    };
    vm.ret_ref(ctx, h);
    Ok(())
}

fn native_get_string_def(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let key = require_string(vm, ctx, 0)?;
    let def = vm.arg_ref(ctx, 1);
    match get(&key) {
        Some(v) => {
            let h = vm.new_string(&v)?;
            vm.ret_ref(ctx, h);
        }
        // The default is passed through untouched (it may be null).
        None => vm.ret_ref(ctx, def),
    }
    Ok(())
}

fn native_get_int_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let key = require_string(vm, ctx, 0)?;
    let def = vm.arg_i32(ctx, 1);
    let raw = get(&key);
    vm.ret_i32(ctx, parse_i32(raw.as_deref(), def));
    Ok(())
}

fn native_get_long_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let key = require_string(vm, ctx, 0)?;
    let def = vm.arg_i64(ctx, 1);
    let raw = get(&key);
    vm.ret_i64(ctx, parse_i64(raw.as_deref(), def));
    Ok(())
}

fn native_get_boolean_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let key = require_string(vm, ctx, 0)?;
    let def = vm.arg_bool(ctx, 1);
    let raw = get(&key);
    vm.ret_bool(ctx, parse_bool(raw.as_deref(), def));
    Ok(())
}

fn native_find(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let key = require_string(vm, ctx, 0)?;
    let handle = lock().find_handle(&key);
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn native_get_handle(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let value = lock().value_for_handle(handle).map(str::to_string);
    match value {
        Some(v) => {
            let h = vm.new_string(&v)?;
            vm.ret_ref(ctx, h);
        }
        // The key was deleted after `find`: the handle resolves to null.
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn native_get_int_handle(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let def = vm.arg_i32(ctx, 1);
    let value = lock().value_for_handle(handle).map(str::to_string);
    vm.ret_i32(ctx, parse_i32(value.as_deref(), def));
    Ok(())
}

fn native_get_long_handle(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let def = vm.arg_i64(ctx, 1);
    let value = lock().value_for_handle(handle).map(str::to_string);
    vm.ret_i64(ctx, parse_i64(value.as_deref(), def));
    Ok(())
}

fn native_get_boolean_handle(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let def = vm.arg_bool(ctx, 1);
    let value = lock().value_for_handle(handle).map(str::to_string);
    vm.ret_bool(ctx, parse_bool(value.as_deref(), def));
    Ok(())
}

fn native_set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let key = require_string(vm, ctx, 0)?;
    let value = vm.arg_ref(ctx, 1);
    let mut table = lock();
    if value == NULL {
        // `SystemProperties.set(key, null)` deletes a runtime property.
        table.unset(&key);
    } else {
        let v = vm.string_to_rust(value).unwrap_or_default();
        // A `ro.*` key is refused, exactly like init; the call still returns
        // normally because the Java signature is `void`.
        table.set(&key, &v);
    }
    Ok(())
}

/// `SystemProperties.addChangeCallback` — the Java side registers a `Runnable`
/// and calls this to ask the property service to notify it. ACOS has no
/// property service, so the callback is accepted and never fired; `get`/`set`
/// inside one process are synchronous.
fn native_add_change_callback(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

/// `native_report_sysprop_change` — tells the property service that a property
/// may have changed. No service, no-op.
fn native_report_sysprop_change(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
####################################
# from generate-common-build-props
####################################
ro.product.system.brand=google
ro.product.system.model=mainline
ro.system.product.cpu.abilist=x86_64,arm64-v8a
ro.system.product.cpu.abilist32=
ro.system.product.cpu.abilist64=x86_64,arm64-v8a
ro.system.build.fingerprint=google/sdk_gphone64_x86_64/emu64xa:14/UE1A.230829.050/12077443:userdebug/dev-keys
ro.build.id=UE1A.230829.050
ro.build.version.sdk=34
ro.build.version.release=14
ro.build.type=userdebug
ro.product.cpu.abi=x86_64
ro.debuggable=1
ro.secure=1
# a comment between values
ro.build.date=Thu Jul 11 18:22:01 UTC 2024
not-a-property-line
=missing-key
ro.build.version.sdk=34
";

    fn sample() -> PropTable {
        let mut t = PropTable::from_build_prop(SAMPLE, SeedSource::Embedded);
        t.derive();
        t
    }

    #[test]
    fn parses_build_prop_and_skips_comments_and_junk() {
        let t = sample();
        assert_eq!(t.source(), &SeedSource::Embedded);
        assert_eq!(t.get("ro.build.id"), Some("UE1A.230829.050"));
        assert_eq!(t.get("ro.build.version.sdk"), Some("34"));
        assert_eq!(t.get("ro.build.date"), Some("Thu Jul 11 18:22:01 UTC 2024"));
        assert_eq!(t.get("ro.system.product.cpu.abilist32"), Some(""));
        assert_eq!(t.get("not-a-property-line"), None);
        assert_eq!(t.get(""), None, "an empty key must be skipped");
        assert!(!t.is_empty());
        assert!(t.len() > 10);
        assert_eq!(
            t.provenance("ro.build.version.sdk"),
            Some(Provenance::BuildProp)
        );
    }

    #[test]
    fn derives_init_computed_properties() {
        let t = sample();
        assert_eq!(
            t.get("ro.build.fingerprint"),
            Some(
                "google/sdk_gphone64_x86_64/emu64xa:14/UE1A.230829.050/12077443:userdebug/dev-keys"
            )
        );
        assert_eq!(
            t.provenance("ro.build.fingerprint"),
            Some(Provenance::Derived)
        );
        assert_eq!(t.get("ro.product.cpu.abilist"), Some("x86_64,arm64-v8a"));
        assert_eq!(t.get("ro.product.cpu.abilist32"), Some(""));
        assert_eq!(
            t.provenance("ro.product.cpu.abilist"),
            Some(Provenance::Derived)
        );
        // Legacy product aliases come from the system partition.
        assert_eq!(t.get("ro.product.brand"), Some("google"));
        assert_eq!(t.get("ro.product.model"), Some("mainline"));
        assert_eq!(t.get("ro.product.name"), None, "not in the sample");
    }

    #[test]
    fn explicit_value_beats_derivation() {
        let text = "ro.build.fingerprint=explicit\nro.system.build.fingerprint=derived\n";
        let mut t = PropTable::from_build_prop(text, SeedSource::Embedded);
        t.derive();
        assert_eq!(t.get("ro.build.fingerprint"), Some("explicit"));
        assert_eq!(
            t.provenance("ro.build.fingerprint"),
            Some(Provenance::BuildProp)
        );
    }

    #[test]
    fn unknown_key_semantics_match_art() {
        let t = sample();
        assert_eq!(
            t.get("acos.does.not.exist"),
            None,
            "get(key) has no default"
        );
        assert_eq!(parse_i32(t.get("acos.does.not.exist"), 42), 42);
        assert_eq!(parse_i64(t.get("acos.does.not.exist"), -7), -7);
        assert!(parse_bool(t.get("acos.does.not.exist"), true));
        assert!(!parse_bool(t.get("acos.does.not.exist"), false));
        assert_eq!(t.find_handle("acos.does.not.exist"), 0);
        assert_eq!(t.value_for_handle(0), None);
        assert_eq!(t.key_for_handle(4242), None);
    }

    #[test]
    fn numeric_and_boolean_parsing_is_strict() {
        assert_eq!(parse_i32(Some("34"), 0), 34);
        assert_eq!(parse_i32(Some(" 34 "), 0), 34);
        assert_eq!(parse_i32(Some("+12"), 0), 12);
        assert_eq!(parse_i32(Some("0x22"), 99), 99, "no hex, like ART");
        assert_eq!(parse_i32(Some("34abc"), 99), 99);
        assert_eq!(parse_i32(Some(""), 99), 99);
        assert_eq!(parse_i32(Some("2147483648"), 99), 99, "must fit in an int");
        assert_eq!(parse_i64(Some("12077443"), 0), 12_077_443);
        assert_eq!(
            parse_i64(Some("9223372036854775808"), 5),
            5,
            "must fit in a long"
        );
        assert!(parse_bool(Some("1"), false));
        assert!(parse_bool(Some("true"), false));
        assert!(parse_bool(Some("on"), false));
        assert!(!parse_bool(Some("off"), true));
        assert!(!parse_bool(Some("false"), true));
        assert!(parse_bool(Some("maybe"), true), "unrecognised -> default");
        assert!(parse_bool(Some("TRUE"), true), "case sensitive -> default");
    }

    #[test]
    fn handle_ids_are_stable_and_survive_writes() {
        let mut t = sample();
        let sdk = t.find_handle("ro.build.version.sdk");
        assert_ne!(sdk, 0);
        assert_eq!(t.key_for_handle(sdk), Some("ro.build.version.sdk"));
        assert_eq!(t.value_for_handle(sdk), Some("34"));

        assert!(t.set("acos.runtime", "rust"));
        let rt = t.find_handle("acos.runtime");
        assert_ne!(rt, 0);
        assert_eq!(t.value_for_handle(rt), Some("rust"));
        assert_eq!(t.value_for_handle(sdk), Some("34"), "ids do not shift");

        assert!(t.unset("acos.runtime"));
        assert_eq!(t.find_handle("acos.runtime"), 0);
        assert_eq!(
            t.value_for_handle(rt),
            None,
            "a deleted key's handle resolves to nothing"
        );
        assert_eq!(t.value_for_handle(sdk), Some("34"));
    }

    #[test]
    fn readonly_properties_cannot_be_written() {
        let mut t = sample();
        assert!(!t.set("ro.build.version.sdk", "35"));
        assert_eq!(t.get("ro.build.version.sdk"), Some("34"));
        assert!(!t.unset("ro.build.version.sdk"));
        assert_eq!(t.get("ro.build.version.sdk"), Some("34"));
        assert!(t.set("persist.sys.locale", "en-US"));
        assert_eq!(t.get("persist.sys.locale"), Some("en-US"));
        assert_eq!(
            t.provenance("persist.sys.locale"),
            Some(Provenance::Runtime)
        );
    }

    #[test]
    fn real_build_prop_matches_the_reference_image() {
        // Unit tests run with CWD = crates/acos-vm; `default_boot_dir` walks up
        // to the repo root, so this exercises the shipped seed.
        if matches!(seed_source(), SeedSource::Embedded) {
            acos_test_support::require_prereq(false, "real_build_prop_matches_the_reference_image", "real_build_prop_matches_the_reference_image: no .cache/boot/android-34/build.prop (embedded ART capture in use)");
            return;
        }
        assert_eq!(get("ro.build.version.sdk").as_deref(), Some("34"));
        assert_eq!(get("ro.build.version.release").as_deref(), Some("14"));
        assert_eq!(get("ro.product.cpu.abi").as_deref(), Some("x86_64"));
        assert_eq!(get("ro.build.type").as_deref(), Some("userdebug"));
        assert_eq!(get("ro.debuggable").as_deref(), Some("1"));
        // Derived, not stored, in the extracted build.prop.
        assert_eq!(
            provenance("ro.build.fingerprint"),
            Some(Provenance::Derived)
        );
        assert_eq!(
            get("ro.build.fingerprint").as_deref(),
            Some(
                "google/sdk_gphone64_x86_64/emu64xa:14/UE1A.230829.050/12077443:userdebug/dev-keys"
            )
        );
        assert_eq!(
            provenance("ro.product.cpu.abilist"),
            Some(Provenance::Derived)
        );
        assert_eq!(
            get("ro.product.cpu.abilist").as_deref(),
            Some("x86_64,arm64-v8a")
        );
        // Build.VERSION.SDK_INT / Build.SUPPORTED_ABIS read through these.
        assert_eq!(parse_i32(get("ro.build.version.sdk").as_deref(), 0), 34);
        let abis: Vec<String> = get("ro.product.cpu.abilist")
            .unwrap()
            .split(',')
            .map(str::to_string)
            .collect();
        assert_eq!(abis, vec!["x86_64", "arm64-v8a"]);
    }

    #[test]
    fn process_table_is_seeded_from_a_real_source() {
        let source = seed_source();
        match &source {
            SeedSource::BuildProp(path) => {
                assert!(path.ends_with("build.prop"), "seeded from {path:?}");
                assert!(len() > 50, "build.prop has ~128 lines");
            }
            SeedSource::Embedded => {
                eprintln!("NOTE process_table_is_seeded_from_a_real_source: embedded capture");
                assert!(len() >= EMBEDDED_PROPS.len());
            }
        }
        assert!(get("ro.build.version.sdk").is_some());
    }
}
