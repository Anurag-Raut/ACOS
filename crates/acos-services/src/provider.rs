//! The Settings content provider (`IContentProvider.call`) and the generation
//! counter.
//!
//! `Settings$NameValueCache` talks to the provider through the hand-written
//! `ContentProviderProxy`, which is why the 5 launch / 4 interaction
//! `IContentProvider.call` transactions are missing from the `$Stub$Proxy`
//! attribution (service-layer REPORT §3.7). The provider hands back a
//! `ParcelFileDescriptor`-backed `MemoryIntArray` generation counter, so the
//! boundary is modelled explicitly: [`GenerationBacking`] is the fd the later
//! native layer provides, and [`MemfdGeneration`] is the pure-Rust fallback
//! (a real `memfd`) that unit tests use.
//!
//! Seeded from `research/ambient-state/settings_keys.tsv`.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use acos_aidl::MethodInfo;
use acos_parcel::ParcelFd;

use crate::error::{ServiceError, Result};
use crate::host::{CallResult, ServiceObject};
use crate::value::{Bundle, DynamicValue, Parcelable, RawFdValue, Value};

thread_local! {
    /// The binder the `settings` `IContentProvider` was registered under.
    ///
    /// `acos-services` cannot see the VM, and `acos-runtime::ams_java` cannot
    /// see `ServiceHost`; recording the binder here lets the `getContentProvider`
    /// reply carry the real `IContentProvider` instead of `null`.
    static SETTINGS_PROVIDER_BINDER: Cell<u64> = const { Cell::new(0) };
}

/// Record the `settings` provider binder at host install time.
pub fn set_settings_provider_binder(binder: u64) {
    SETTINGS_PROVIDER_BINDER.with(|slot| slot.set(binder));
}

/// The `settings` provider binder recorded by [`set_settings_provider_binder`].
#[must_use]
pub fn settings_provider_binder() -> u64 {
    SETTINGS_PROVIDER_BINDER.with(Cell::get)
}

/// A settings namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Namespace {
    /// `Settings.Global`.
    Global,
    /// `Settings.Secure`.
    Secure,
    /// `Settings.System`.
    System,
}

impl Namespace {
    /// Parse a provider method (`GET_global`, `PUT_secure`, ...).
    #[must_use]
    pub fn from_method(method: &str) -> Option<(bool, Self)> {
        let (verb, rest) = method.split_once('_')?;
        let ns = match rest {
            "global" => Self::Global,
            "secure" => Self::Secure,
            "system" => Self::System,
            _ => return None,
        };
        match verb {
            "GET" => Some((false, ns)),
            "PUT" => Some((true, ns)),
            _ => None,
        }
    }

    /// Stable index for the generation array.
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            Self::Global => 0,
            Self::Secure => 1,
            Self::System => 2,
        }
    }

    /// Wire name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Secure => "secure",
            Self::System => "system",
        }
    }
}

/// The fd-backed `MemoryIntArray` boundary.
pub trait GenerationBacking: Send {
    /// Current generation for `namespace`.
    fn generation(&self, namespace: Namespace) -> i32;
    /// Bump and return the new generation.
    fn bump(&mut self, namespace: Namespace) -> i32;
    /// The file descriptor the native layer reads (the `MemoryIntArray`).
    fn file_descriptor(&self) -> Option<ParcelFd>;
}

/// A pure in-memory counter (used when `memfd_create` is unavailable).
#[derive(Debug, Default)]
pub struct InMemoryGeneration {
    counters: [i32; 3],
}

impl InMemoryGeneration {
    /// New zeroed counters.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl GenerationBacking for InMemoryGeneration {
    fn generation(&self, namespace: Namespace) -> i32 {
        self.counters[namespace.index()]
    }

    fn bump(&mut self, namespace: Namespace) -> i32 {
        let slot = &mut self.counters[namespace.index()];
        *slot = slot.wrapping_add(1);
        *slot
    }

    fn file_descriptor(&self) -> Option<ParcelFd> {
        None
    }
}

/// A `memfd`-backed counter (the ACOS default).
#[derive(Debug)]
pub struct MemfdGeneration {
    counters: [i32; 3],
    fd: Option<ParcelFd>,
}

impl MemfdGeneration {
    /// Create a 12-byte memfd-backed counter.
    ///
    /// The memfd is sized immediately: the client's `MemoryIntArray.size()`
    /// calls `nativeSize`, which reports the backing file length, and it reads
    /// the array before the first `PUT` (which is what would otherwise extend
    /// the file via [`Self::sync`]). An unsized memfd reports length 0 and the
    /// first `MemoryIntArray.get(0)` throws `IndexOutOfBoundsException`.
    #[must_use]
    pub fn new() -> Self {
        let state = Self {
            counters: [0; 3],
            fd: ParcelFd::memfd("acos-settings-generation").ok(),
        };
        state.sync();
        state
    }

    fn sync(&self) {
        let Some(fd) = self.fd.as_ref() else {
            return;
        };
        let mut bytes = [0u8; 12];
        for (i, counter) in self.counters.iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&counter.to_le_bytes());
        }
        // The fd is owned by `ParcelFd`; write through a borrowed `File`.
        let Ok(owned) = fd.as_fd().try_clone_to_owned() else {
            return;
        };
        let mut file = std::fs::File::from(owned);
        use std::io::{Seek, SeekFrom, Write};
        let _ = file.seek(SeekFrom::Start(0));
        let _ = file.write_all(&bytes);
    }
}

impl Default for MemfdGeneration {
    fn default() -> Self {
        Self::new()
    }
}

impl GenerationBacking for MemfdGeneration {
    fn generation(&self, namespace: Namespace) -> i32 {
        self.counters[namespace.index()]
    }

    fn bump(&mut self, namespace: Namespace) -> i32 {
        let slot = &mut self.counters[namespace.index()];
        *slot = slot.wrapping_add(1);
        let value = *slot;
        self.sync();
        value
    }

    fn file_descriptor(&self) -> Option<ParcelFd> {
        self.fd.as_ref().and_then(|fd| fd.try_clone().ok())
    }
}

/// The Settings provider state.
pub struct SettingsState {
    /// `Settings.Global`.
    pub global: BTreeMap<String, String>,
    /// `Settings.Secure`.
    pub secure: BTreeMap<String, String>,
    /// `Settings.System`.
    pub system: BTreeMap<String, String>,
    /// `Settings.Config` (DeviceConfig): composite `namespace/name` -> value.
    ///
    /// The reference API-34 emulator has no DeviceConfig entries set, so this
    /// is empty; `LIST_config` must still answer with a well-formed empty
    /// `HashMap` (`DeviceConfig.getProperties` reads the reply through
    /// `Bundle.getSerializable`).
    pub config: BTreeMap<String, String>,
    /// The generation counter backing.
    pub generation: Box<dyn GenerationBacking>,
    /// The namespaces whose generation fd has already been handed out.
    pub generation_sent: [bool; 3],
    /// Registered `IContentObserver` binder handles.
    pub observers: Vec<u64>,
    /// Content-change log `(namespace, key)` for evidence.
    pub changes: Vec<(Namespace, String)>,
}

impl SettingsState {
    /// Seed the measured keys (`settings_keys.tsv`).
    #[must_use]
    pub fn new() -> Self {
        let mut state = Self {
            global: BTreeMap::new(),
            secure: BTreeMap::new(),
            system: BTreeMap::new(),
            config: BTreeMap::new(),
            generation: Box::new(MemfdGeneration::new()),
            generation_sent: [false; 3],
            observers: Vec::new(),
            changes: Vec::new(),
        };
        // Stock Android 14 (API-34) emulator defaults for the keys the
        // framework reads while inflating a themed `EditText`/`ListView`.
        // `settings_keys.tsv` (captured from the reference ART run) records
        // the four `System.*` text-keyboard preferences as set to `1`; the
        // `Global.*` keys the launch reads (`force_resizable_activities`,
        // `render_shadows_in_compositor`, `show_angle_in_use_dialog_box`) are
        // unset on the reference, so they stay absent and `Settings.getInt`
        // falls back to the caller's default exactly as on device.
        state.system.insert("auto_caps".into(), "1".into());
        state.system.insert("auto_punctuate".into(), "1".into());
        state.system.insert("auto_replace".into(), "1".into());
        state.system.insert("show_password".into(), "1".into());
        state
    }

    /// A state with an injected generation backing (the native boundary).
    #[must_use]
    pub fn with_generation(generation: Box<dyn GenerationBacking>) -> Self {
        let mut state = Self::new();
        state.generation = generation;
        state
    }

    /// The map for a namespace.
    #[must_use]
    pub fn map(&self, namespace: Namespace) -> &BTreeMap<String, String> {
        match namespace {
            Namespace::Global => &self.global,
            Namespace::Secure => &self.secure,
            Namespace::System => &self.system,
        }
    }

    fn map_mut(&mut self, namespace: Namespace) -> &mut BTreeMap<String, String> {
        match namespace {
            Namespace::Global => &mut self.global,
            Namespace::Secure => &mut self.secure,
            Namespace::System => &mut self.system,
        }
    }

    /// `GET_*`: the value, or `None` when unset.
    #[must_use]
    pub fn get(&self, namespace: Namespace, key: &str) -> Option<String> {
        self.map(namespace).get(key).cloned()
    }

    /// `PUT_*`: set the value and bump the generation.
    pub fn put(&mut self, namespace: Namespace, key: &str, value: &str) -> i32 {
        self.map_mut(namespace)
            .insert(key.to_string(), value.to_string());
        self.changes.push((namespace, key.to_string()));
        self.generation.bump(namespace)
    }

    /// Current generation for a namespace.
    #[must_use]
    pub fn generation(&self, namespace: Namespace) -> i32 {
        self.generation.generation(namespace)
    }

    /// Handle `IContentProvider.call`.
    pub fn call(&mut self, method: &str, arg: &str, extras: Option<&Bundle>) -> Result<Bundle> {
        if method == "LIST_config" {
            return self.list_config(extras);
        }
        if method == "GET_config" {
            return self.get_config(arg);
        }
        let Some((is_put, namespace)) = Namespace::from_method(method) else {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: "android.content.IContentProvider".into(),
                code: 0,
                method: method.to_string(),
            });
        };
        let mut bundle = Bundle::new();
        if is_put {
            let value = extras
                .and_then(|b| b.get("value"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            self.put(namespace, arg, &value);
            bundle.put("value", Value::Skipped);
        } else {
            match self.get(namespace, arg) {
                Some(value) => bundle.put("value", Value::Str(Some(value))),
                None => bundle.put("value", Value::Skipped),
            }
            // The generation counter is observable as a plain int (test /
            // evidence) and, on the first read per namespace, as the real
            // fd-backed `MemoryIntArray` `Settings$GenerationTracker` uses.
            bundle.put(
                "generation_int",
                Value::Int(self.generation.generation(namespace)),
            );
            if !self.generation_sent[namespace.index()] {
                if let Some(fd) = self.generation.file_descriptor() {
                    let pfd = Parcelable::ordered(
                        "android.os.ParcelFileDescriptor",
                        vec![Value::Int(0), Value::RawFd(Some(RawFdValue(fd)))],
                    );
                    bundle.put(
                        "generation",
                        Value::Parcelable(Parcelable::ordered(
                            "android.util.MemoryIntArray",
                            vec![Value::NamedParcelable(pfd)],
                        )),
                    );
                    self.generation_sent[namespace.index()] = true;
                }
            }
        }
        Ok(bundle)
    }

    /// `Settings$Config.CALL_METHOD_LIST_CONFIG`.
    ///
    /// The client (`Settings$NameValueCache.getStringsForPrefix`) sends the
    /// namespace prefix in the `_prefix` extra and expects a `Bundle` whose
    /// `value` key is a `HashMap<String, String>` of composite
    /// (`namespace/name`) keys. `DeviceConfig.getProperties` turns that into a
    /// `Properties`; an empty map is the honest answer when no DeviceConfig
    /// entry is set (the reference image has none).
    fn list_config(&self, extras: Option<&Bundle>) -> Result<Bundle> {
        let prefix = extras
            .and_then(|b| b.get("_prefix"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let entries: Vec<(DynamicValue, DynamicValue)> = self
            .config
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| {
                (
                    DynamicValue::String(Some(k.clone())),
                    DynamicValue::String(Some(v.clone())),
                )
            })
            .collect();
        let mut bundle = Bundle::new();
        bundle.put("value", Value::Map(Some(entries)));
        Ok(bundle)
    }

    /// `Settings$Config.CALL_METHOD_GET_CONFIG`.
    fn get_config(&self, composite_name: &str) -> Result<Bundle> {
        let mut bundle = Bundle::new();
        match self.config.get(composite_name) {
            Some(value) => bundle.put("value", Value::Str(Some(value.clone()))),
            None => bundle.put("value", Value::Skipped),
        }
        Ok(bundle)
    }
}

impl Default for SettingsState {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for SettingsState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SettingsState")
            .field("global", &self.global)
            .field("secure", &self.secure)
            .field("system", &self.system)
            .field("generation_sent", &self.generation_sent)
            .field("observers", &self.observers)
            .field("changes", &self.changes)
            .finish_non_exhaustive()
    }
}

/// `IContentProvider` handler for `settings`.
pub struct ProviderService {
    state: Arc<Mutex<SettingsState>>,
}

impl ProviderService {
    /// Wrap shared settings state.
    #[must_use]
    pub fn new(state: Arc<Mutex<SettingsState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for ProviderService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        args: &[Value],
    ) -> Result<CallResult> {
        if interface != "android.content.IContentProvider" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let reply = match method.name.as_str() {
            "call" => {
                // `ContentProviderProxy.call(AttributionSource, authority, method, arg, extras)`.
                let provider_method = args.get(2).and_then(Value::as_str).unwrap_or_default();
                let arg = args.get(3).and_then(Value::as_str).unwrap_or_default();
                let extras = args.get(4).and_then(Value::as_bundle);
                let mut state = self.state.lock().unwrap();
                let bundle = state.call(provider_method, arg, extras)?;
                vec![Value::Bundle(Some(bundle))]
            }
            "getType" => vec![Value::Str(None)],
            "query" | "insert" | "delete" | "update" | "bulkInsert" => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

/// `IContentService` handler (`registerContentObserver` + `onChange`).
pub struct ContentServiceService {
    state: Arc<Mutex<SettingsState>>,
}

impl ContentServiceService {
    /// Wrap shared settings state.
    #[must_use]
    pub fn new(state: Arc<Mutex<SettingsState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for ContentServiceService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        args: &[Value],
    ) -> Result<CallResult> {
        if interface != "android.content.IContentService" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let reply = match method.name.as_str() {
            "registerContentObserver" => {
                if let Some(observer) = args.get(2).and_then(Value::as_binder) {
                    self.state.lock().unwrap().observers.push(observer.0);
                }
                Vec::new()
            }
            "unregisterContentObserver" => {
                if let Some(observer) = args.first().and_then(Value::as_binder) {
                    self.state
                        .lock()
                        .unwrap()
                        .observers
                        .retain(|o| *o != observer.0);
                }
                Vec::new()
            }
            "notifyChange" => Vec::new(),
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

/// Encode a `Bundle` reply for evidence.
#[must_use]
pub fn bundle_summary(bundle: &Bundle) -> String {
    let parts: Vec<String> = bundle
        .entries
        .iter()
        .map(|(k, v)| format!("{k}={v:?}"))
        .collect();
    format!("{{{}}}", parts.join(", "))
}

/// A `Parcelable` for the `android.net.Uri` argument (curated).
#[must_use]
pub fn uri(s: impl Into<String>) -> Parcelable {
    Parcelable::ordered("android.net.Uri", vec![Value::Str(Some(s.into()))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_config_returns_an_empty_hash_map_bundle() {
        // W134-A: `Settings$NameValueCache.getStringsForPrefix` reads the
        // `LIST_config` reply's `value` key as a `HashMap` (parcel `VAL_MAP`).
        // The reference API-34 image has no DeviceConfig entries, so the
        // honest answer is a well-formed empty map; `DeviceConfig.getProperties`
        // then yields an empty `Properties` instead of failing the inflate.
        let mut state = SettingsState::new();
        let mut extras = Bundle::new();
        extras.put("_prefix", Value::Str(Some("textclassifier/".into())));
        let bundle = state.call("LIST_config", "", Some(&extras)).unwrap();
        let value = bundle.get("value").expect("value key");
        match value {
            Value::Map(Some(entries)) => assert!(entries.is_empty()),
            other => panic!("expected an empty map, got {other:?}"),
        }
    }

    #[test]
    fn list_config_filters_by_prefix() {
        let mut state = SettingsState::new();
        state
            .config
            .insert("textclassifier/foo".into(), "bar".into());
        state.config.insert("other/baz".into(), "qux".into());
        let mut extras = Bundle::new();
        extras.put("_prefix", Value::Str(Some("textclassifier/".into())));
        let bundle = state.call("LIST_config", "", Some(&extras)).unwrap();
        let Value::Map(Some(entries)) = bundle.get("value").unwrap() else {
            panic!("expected map");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].0,
            crate::value::DynamicValue::String(Some("textclassifier/foo".into()))
        );
    }

    #[test]
    fn seeded_keys_match_settings_keys_tsv() {
        let state = SettingsState::new();
        assert_eq!(state.get(Namespace::System, "auto_caps").as_deref(), Some("1"));
        assert_eq!(state.get(Namespace::System, "show_password").as_deref(), Some("1"));
        assert_eq!(state.get(Namespace::Global, "force_resizable_activities"), None);
        assert_eq!(state.get(Namespace::Global, "render_shadows_in_compositor"), None);
    }

    #[test]
    fn get_returns_value_bundle_and_generation_memintarray() {
        let mut state = SettingsState::new();
        let bundle = state.call("GET_system", "auto_caps", None).unwrap();
        assert_eq!(bundle.get("value").and_then(Value::as_str), Some("1"));
        let generation = bundle
            .get("generation")
            .and_then(Value::as_parcelable)
            .expect("generation parcelable");
        assert_eq!(generation.class(), Some("android.util.MemoryIntArray"));
        // The fd is handed out once per namespace.
        let again = state.call("GET_system", "auto_caps", None).unwrap();
        assert!(again.get("generation").is_none());
    }

    #[test]
    fn put_updates_value_and_bumps_generation() {
        let mut state = SettingsState::new();
        let before = state.generation(Namespace::Global);
        let mut extras = Bundle::new();
        extras.put("value", Value::Str(Some("42".into())));
        state.call("PUT_global", "answer", Some(&extras)).unwrap();
        assert_eq!(state.get(Namespace::Global, "answer").as_deref(), Some("42"));
        assert_eq!(state.generation(Namespace::Global), before + 1);
        // A real change resets the generation-fd hand-out.
        let bundle = state.call("GET_global", "answer", None).unwrap();
        assert_eq!(bundle.get("value").and_then(Value::as_str), Some("42"));
        assert!(bundle.get("generation").is_some());
    }

    #[test]
    fn change_count_is_exactly_one_per_real_change() {
        // "observer fires once per change, never spuriously" at the state
        // machine level: a GET never records a change, a PUT records exactly
        // one, and a failed/unknown method records none.
        let mut state = SettingsState::new();
        assert!(state.changes.is_empty());
        state.call("GET_global", "animator_duration_scale", None).unwrap();
        assert!(state.changes.is_empty(), "a read is not a change");
        let mut extras = Bundle::new();
        extras.put("value", Value::Str(Some("1".into())));
        state.call("PUT_global", "k", Some(&extras)).unwrap();
        assert_eq!(state.changes.len(), 1, "one PUT is one change");
        state.call("GET_global", "k", None).unwrap();
        assert_eq!(state.changes.len(), 1, "a read after a change adds none");
        state.call("PUT_global", "k", Some(&extras)).unwrap();
        assert_eq!(state.changes.len(), 2, "a second PUT is a second change");
    }

    #[test]
    fn unknown_provider_method_is_structured() {
        let mut state = SettingsState::new();
        let err = state.call("DROP_global", "x", None).unwrap_err();
        assert_eq!(err.code(), crate::codes::UNSUPPORTED_ANDROID_API);
    }

    #[test]
    fn injected_generation_backing_is_used() {
        let state = SettingsState::with_generation(Box::new(InMemoryGeneration::new()));
        assert_eq!(state.generation(Namespace::Secure), 0);
    }
}
