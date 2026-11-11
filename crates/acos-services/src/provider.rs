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
use acos_parcel::{Parcel, ParcelFd};

use crate::cursor::{CursorResult, Field, QueryRequest};
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

/// The seeded `Settings` / `DeviceConfig` tables.
///
/// The values are the stock API-34 emulator's (the reference ART image) and are
/// only defaults: a harness overrides any of them with [`SettingsState::seed`] /
/// [`SettingsState::seed_config`], and the runtime can pass a whole map through
/// [`SettingsState::with_defaults`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsDefaults {
    /// `Settings.Global` rows.
    pub global: BTreeMap<String, String>,
    /// `Settings.Secure` rows.
    pub secure: BTreeMap<String, String>,
    /// `Settings.System` rows.
    pub system: BTreeMap<String, String>,
    /// `DeviceConfig` rows, composite `namespace/name`.
    pub config: BTreeMap<String, String>,
}

impl SettingsDefaults {
    /// A stock Android 14 (API-34) emulator image.
    ///
    /// Values an `adb shell settings` / `adb shell device_config` session would
    /// show on a freshly booted emulator, including the `ime/` DeviceConfig
    /// namespace `Settings$Config.getStrings` reads while the IME initialises
    /// (the path `hello-views`'s `EditText` blocked on, P2a §4).
    #[must_use]
    pub fn api34() -> Self {
        let mut defaults = Self::default();
        for (key, value) in [
            // Provisioned, debuggable emulator.
            ("device_provisioned", "1"),
            ("adb_enabled", "1"),
            ("development_settings_enabled", "1"),
            ("airplane_mode_on", "0"),
            ("auto_time", "1"),
            ("auto_time_zone", "1"),
            ("wifi_on", "1"),
            ("bluetooth_on", "0"),
            ("stay_on_while_plugged_in", "0"),
            ("package_verifier_enable", "1"),
            ("boot_count", "0"),
        ] {
            defaults.global.insert(key.into(), value.into());
        }
        for (key, value) in [
            ("android_id", "acos0000000000ff"),
            ("user_setup_complete", "1"),
            ("install_non_market_apps", "0"),
            ("mock_location", "0"),
            ("accessibility_enabled", "0"),
            ("touch_exploration_enabled", "0"),
            ("bluetooth_name", "ACOS"),
            ("bluetooth_address", "02:00:00:00:00:00"),
            ("default_input_method", "com.android.inputmethod.latin/.LatinIME"),
            (
                "enabled_input_methods",
                "com.android.inputmethod.latin/.LatinIME",
            ),
            ("show_ime_with_hard_keyboard", "0"),
            ("selected_input_method_subtype", "-1"),
        ] {
            defaults.secure.insert(key.into(), value.into());
        }
        for (key, value) in [
            ("screen_brightness", "102"),
            ("screen_brightness_mode", "1"),
            ("screen_off_timeout", "60000"),
            ("sound_effects_enabled", "0"),
            ("vibrate_when_ringing", "1"),
            ("haptic_feedback_enabled", "1"),
            ("font_scale", "1.0"),
            ("accelerometer_rotation", "1"),
            ("user_rotation", "0"),
            ("time_12_24", "12"),
            ("date_format", ""),
            ("text_show_password", "0"),
        ] {
            defaults.system.insert(key.into(), value.into());
        }
        // `DeviceConfig` as a stock emulator has it: the IME namespace exists
        // (the framework asks for it), the textclassifier one does not.
        for (key, value) in [
            ("ime/enabled_input_methods", "com.android.inputmethod.latin/.LatinIME"),
            ("ime/default_input_method", "com.android.inputmethod.latin/.LatinIME"),
            ("ime/show_ime_with_hard_keyboard", "false"),
        ] {
            defaults.config.insert(key.into(), value.into());
        }
        defaults
    }

    /// Merge `other` over `self` (harness overrides).
    pub fn override_with(&mut self, other: &SettingsDefaults) {
        self.global.extend(other.global.clone());
        self.secure.extend(other.secure.clone());
        self.system.extend(other.system.clone());
        self.config.extend(other.config.clone());
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
    /// The API-34 device defaults (a stock emulator image).
    #[must_use]
    pub fn new() -> Self {
        Self::with_defaults(SettingsDefaults::api34())
    }

    /// A state seeded from `defaults` (the configurable map the harness can
    /// point at a different device or test).
    #[must_use]
    pub fn with_defaults(defaults: SettingsDefaults) -> Self {
        let mut state = Self {
            global: defaults.global,
            secure: defaults.secure,
            system: defaults.system,
            config: defaults.config,
            generation: Box::new(MemfdGeneration::new()),
            generation_sent: [false; 3],
            observers: Vec::new(),
            changes: Vec::new(),
        };
        // `settings_keys.tsv` (captured from the reference ART run) records the
        // four `System.*` text-keyboard preferences as set to `1`; the
        // `Global.*` keys the launch reads (`force_resizable_activities`,
        // `render_shadows_in_compositor`, `show_angle_in_use_dialog_box`) are
        // unset on the reference, so they stay absent and `Settings.getInt`
        // falls back to the caller's default exactly as on device.
        for (key, value) in [
            ("auto_caps", "1"),
            ("auto_punctuate", "1"),
            ("auto_replace", "1"),
            ("show_password", "1"),
        ] {
            state
                .system
                .entry(key.to_string())
                .or_insert_with(|| value.to_string());
        }
        state
    }

    /// Override one seeded value without recording a change (harness setup).
    pub fn seed(&mut self, namespace: Namespace, key: &str, value: &str) {
        self.map_mut(namespace)
            .insert(key.to_string(), value.to_string());
    }

    /// Override one DeviceConfig value (`namespace/name`).
    pub fn seed_config(&mut self, composite_name: &str, value: &str) {
        self.config
            .insert(composite_name.to_string(), value.to_string());
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

    /// `IContentProvider.query` for the `settings` authority.
    ///
    /// Supported URIs and arguments are the ones the framework itself sends:
    /// `content://settings/<system|secure|global>` with `projection` (`_id`,
    /// `name`, `value`; `null` means all three), the SQL selection `name=?`
    /// (+ one argument) or none, and the `content://settings/config` table with
    /// `name=?` in composite `namespace/name` form. Anything else is a
    /// structured [`ServiceError::UnsupportedAndroidApi`], never an empty
    /// cursor that would read like "the key does not exist".
    pub fn query(
        &self,
        uri: &str,
        projection: Option<&[String]>,
        query_args: Option<&Bundle>,
    ) -> Result<CursorResult> {
        let table = uri
            .strip_prefix("content://settings/")
            .ok_or_else(|| invalid_uri(uri))?;
        let (table, key_from_path) = match table.split_once('/') {
            Some((table, item)) => (table, Some(item.to_string())),
            None => (table, None),
        };

        let columns = projection_columns(projection)?;
        let selection = selection_key(query_args)?;
        if key_from_path.is_some() && selection.is_some() {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: "android.content.IContentProvider".into(),
                code: 1,
                method: "query(item uri with a name= ? selection)".into(),
            });
        }
        let wanted = key_from_path.or(selection);

        let rows: Vec<(&str, &str)> = match table {
            "system" | "secure" | "global" => {
                let namespace = Namespace::from_method(&format!("GET_{table}"))
                    .map(|(_, ns)| ns)
                    .ok_or_else(|| invalid_uri(uri))?;
                self.map(namespace)
                    .iter()
                    .filter(|(name, _)| wanted.as_ref().is_none_or(|key| key == *name))
                    .map(|(name, value)| (name.as_str(), value.as_str()))
                    .collect()
            }
            "config" => self
                .config
                .iter()
                .filter(|(name, _)| wanted.as_ref().is_none_or(|key| key == *name))
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect(),
            other => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: "android.content.IContentProvider".into(),
                    code: 1,
                    method: format!("query(table {other})"),
                })
            }
        };

        let width = columns.len();
        let mut out_rows = Vec::with_capacity(rows.len());
        for (index, (name, value)) in rows.iter().enumerate() {
            let mut row = Vec::with_capacity(width);
            for column in &columns {
                row.push(match column.as_str() {
                    "_id" => Field::Long(i64::try_from(index + 1).unwrap_or(i64::MAX)),
                    "name" => Field::text(*name),
                    "value" => Field::text(*value),
                    _ => unreachable!("projection validated above"),
                });
            }
            out_rows.push(row);
        }
        Ok(CursorResult::new(columns, out_rows))
    }
}

/// `content://settings/<table>` with no table is not a query the provider
/// serves; say exactly that instead of returning an empty cursor.
fn invalid_uri(uri: &str) -> ServiceError {
    ServiceError::BadRequest {
        interface: "android.content.IContentProvider".into(),
        method: "query".into(),
        detail: format!("not a settings content uri: {uri}"),
    }
}

/// The columns of a settings row (`Settings.NameValueTable`: `_id`, `name`,
/// `value`). `null` or an empty projection means all of them, exactly like
/// `SQLiteQueryBuilder` + `ContentProvider.query`.
fn projection_columns(projection: Option<&[String]>) -> Result<Vec<String>> {
    let all = ["_id", "name", "value"];
    let Some(projection) = projection.filter(|p| !p.is_empty()) else {
        return Ok(all.iter().map(|c| (*c).to_string()).collect());
    };
    let mut columns = Vec::with_capacity(projection.len());
    for column in projection {
        if !all.contains(&column.as_str()) {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: "android.content.IContentProvider".into(),
                code: 1,
                method: format!("query(projection column {column})"),
            });
        }
        columns.push(column.clone());
    }
    Ok(columns)
}

/// `ContentResolver.createSqlQueryBundle("name=?", [key], null)` → the key.
///
/// The framework's `Settings$NameValueCache` uses exactly this selection for a
/// single-row read; anything richer is a structured
/// [`ServiceError::UnsupportedAndroidApi`] rather than a silently ignored
/// filter (which would read as "the key does not exist").
fn selection_key(query_args: Option<&Bundle>) -> Result<Option<String>> {
    const SELECTION: &str = "android:query-arg-sql-selection";
    const SELECTION_ARGS: &str = "android:query-arg-sql-selection-args";
    let Some(query_args) = query_args else {
        return Ok(None);
    };
    let selection = query_args.get(SELECTION).and_then(Value::as_str);
    let Some(selection) = selection else {
        // No SQL selection: the caller wants every row of the table.
        if query_args.get(SELECTION_ARGS).is_some() {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: "android.content.IContentProvider".into(),
                code: 1,
                method: "query(selection args without a selection)".into(),
            });
        }
        return Ok(None);
    };
    let normalised: String = selection.chars().filter(|c| !c.is_whitespace()).collect();
    if normalised != "name=?" {
        return Err(ServiceError::UnsupportedAndroidApi {
            interface: "android.content.IContentProvider".into(),
            code: 1,
            method: format!("query(selection {selection:?})"),
        });
    }
    let args = match query_args.get(SELECTION_ARGS) {
        Some(Value::StringArray(Some(args))) => args,
        _ => {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: "android.content.IContentProvider".into(),
                code: 1,
                method: "query(name=? without selection args)".into(),
            })
        }
    };
    match args.as_slice() {
        [Some(key)] => Ok(Some(key.clone())),
        other => Err(ServiceError::UnsupportedAndroidApi {
            interface: "android.content.IContentProvider".into(),
            code: 1,
            method: format!("query(name=? with {} arguments)", other.len()),
        }),
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

    /// `IContentProvider.query` for the `settings` authority.
    ///
    /// The request is decoded with the framework's own `ContentProviderNative`
    /// layout ([`QueryRequest`]), not the generated AIDL table (which records
    /// the projection as a single `Str16`); the host then writes the
    /// `BulkCursorDescriptor` + `CursorWindow` reply and allocates the
    /// `IBulkCursor` binder.
    fn content_query(
        &mut self,
        interface: &str,
        data: &mut Parcel,
    ) -> Result<Option<CursorResult>> {
        if interface != "android.content.IContentProvider" {
            return Ok(None);
        }
        let request = QueryRequest::read(data)?;
        let state = self.state.lock().map_err(|_| ServiceError::Internal(
            "settings state poisoned".into(),
        ))?;
        Ok(Some(state.query(
            &request.uri,
            request.projection.as_deref(),
            request.query_args.as_ref(),
        )?))
    }
}

/// One content provider the host serves.
///
/// `authority` is the key the framework asks `IActivityManager.getContentProvider`
/// for; `binder` is the `IContentProvider` handle `ServiceHost` dispatches to and
/// `connection` is the `ContentProviderConnection` token handed back in the
/// holder (the framework only ever writes it back to
/// `removeContentProvider`/`refContentProvider`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderEntry {
    /// `android:authorities`.
    pub authority: String,
    /// Owning package (`com.android.providers.settings`).
    pub package: String,
    /// Provider class (`SettingsProvider`).
    pub class: String,
    /// `ProviderInfo.processName`.
    pub process_name: String,
    /// `ProviderInfo.readPermission`.
    pub read_permission: String,
    /// `ProviderInfo.writePermission`.
    pub write_permission: String,
    /// `ProviderInfo.exported`.
    pub exported: bool,
    /// `ProviderInfo.grantUriPermissions`.
    pub grant_uri_permissions: bool,
    /// `ProviderInfo.initOrder`.
    pub init_order: i32,
    /// The `android.content.IContentProvider` binder.
    pub binder: u64,
    /// The `ContentProviderConnection` binder (`0` when untracked).
    pub connection: u64,
    /// `ContentProviderHolder.noReleaseNeeded`.
    pub no_release_needed: bool,
}

/// The structured record of one provider transaction.
///
/// Every `getContentProvider`, `getContentProviderExternal`,
/// `removeContentProvider`, `removeContentProviderExternal` and
/// `refContentProvider` appends one of these, so a miss is evidence (a null
/// holder) instead of an unexplained null.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderLookup {
    /// `getContentProvider`, `removeContentProvider`, …
    pub method: String,
    /// The authority asked for (empty when the call carried a connection only).
    pub authority: String,
    /// `callingPackage` (`""` when the transaction does not carry one).
    pub caller: String,
    /// Whether the registry answered with a provider.
    pub hit: bool,
    /// The `IContentProvider` binder in the holder (`0` on a miss).
    pub binder: u64,
    /// The stable reference count after this transaction.
    pub refcount: i32,
}

/// The authorities the host serves, and their reference counts.
///
/// This is the registry `IActivityManager.getContentProvider` consults. It is a
/// plain value (no I/O): a miss appends a [`ProviderLookup`] with `hit: false`
/// and the caller returns a `null` holder, which is what Android returns for an
/// unknown authority.
/// The most provider lookups [`ProviderRegistry`] retains. A guest that loops on
/// `getContentProvider` must not be able to grow the host's heap through the
/// evidence log (the same class of bug as the logd sink: `R1`).
pub const MAX_PROVIDER_LOOKUPS: usize = 10_000;

#[derive(Debug, Default)]
pub struct ProviderRegistry {
    entries: BTreeMap<String, ProviderEntry>,
    refcounts: BTreeMap<String, i32>,
    /// `ContentProviderConnection` binder -> authority.
    connections: BTreeMap<u64, String>,
    lookups: Vec<ProviderLookup>,
    /// Lookups evicted from the front of `lookups` once it hit the cap.
    lookups_dropped: u64,
}

impl ProviderRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Serve `entry` under its authority.
    pub fn register(&mut self, entry: ProviderEntry) {
        if entry.connection != 0 {
            self.connections
                .insert(entry.connection, entry.authority.clone());
        }
        self.refcounts.entry(entry.authority.clone()).or_insert(0);
        self.entries.insert(entry.authority.clone(), entry);
    }

    /// The entry for `authority`, if served.
    #[must_use]
    pub fn get(&self, authority: &str) -> Option<&ProviderEntry> {
        self.entries.get(authority)
    }

    /// Served authorities (sorted).
    #[must_use]
    pub fn authorities(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }

    /// The stable reference count for `authority`.
    #[must_use]
    pub fn refcount(&self, authority: &str) -> i32 {
        self.refcounts.get(authority).copied().unwrap_or(0)
    }

    /// Append a lookup to the evidence log, dropping the oldest once the cap is
    /// reached and counting the drops (never growing without bound).
    fn record(&mut self, lookup: ProviderLookup) {
        if self.lookups.len() >= MAX_PROVIDER_LOOKUPS {
            let drop_n = (self.lookups.len() / 8).max(1);
            self.lookups.drain(..drop_n);
            self.lookups_dropped = self.lookups_dropped.saturating_add(drop_n as u64);
        }
        self.lookups.push(lookup);
    }

    /// `getContentProvider(…)` / `getContentProviderExternal(…)`: the entry plus
    /// a bumped reference count, or `None` with a recorded miss.
    pub fn acquire(
        &mut self,
        method: &str,
        authority: &str,
        caller: &str,
    ) -> Option<ProviderEntry> {
        let entry = self.entries.get(authority).cloned();
        let (hit, binder, refcount) = match &entry {
            Some(entry) => {
                let count = self.refcounts.entry(authority.to_string()).or_insert(0);
                *count += 1;
                (true, entry.binder, *count)
            }
            None => (false, 0, 0),
        };
        self.record(ProviderLookup {
            method: method.to_string(),
            authority: authority.to_string(),
            caller: caller.to_string(),
            hit,
            binder,
            refcount,
        });
        entry
    }

    /// `removeContentProvider`/`removeContentProviderExternal`: drop one stable
    /// reference. Returns the authority the connection/name resolved to.
    pub fn release(
        &mut self,
        method: &str,
        authority: Option<&str>,
        connection: u64,
        caller: &str,
    ) -> Option<String> {
        let resolved = authority
            .map(str::to_string)
            .or_else(|| self.connections.get(&connection).cloned());
        let (hit, binder, refcount) = match resolved.as_deref().and_then(|a| {
            self.entries.get(a).map(|e| (a, e.binder))
        }) {
            Some((authority, binder)) => {
                let count = self.refcounts.entry(authority.to_string()).or_insert(0);
                *count = (*count - 1).max(0);
                (true, binder, *count)
            }
            None => (false, 0, 0),
        };
        self.record(ProviderLookup {
            method: method.to_string(),
            authority: resolved.clone().unwrap_or_default(),
            caller: caller.to_string(),
            hit,
            binder,
            refcount,
        });
        resolved
    }

    /// `refContentProvider(IBinder connection, int stable, int unstable)`.
    ///
    /// `true` when the connection belongs to a served authority (the framework
    /// asserts on a `false` reply), after adjusting the count by
    /// `stable + unstable`.
    pub fn ref_content_provider(&mut self, connection: u64, stable: i32, unstable: i32) -> bool {
        let authority = self.connections.get(&connection).cloned();
        let (hit, binder, refcount) = match authority.as_deref().and_then(|a| {
            self.entries.get(a).map(|e| (a, e.binder))
        }) {
            Some((authority, binder)) => {
                let count = self.refcounts.entry(authority.to_string()).or_insert(0);
                *count = (*count + stable + unstable).max(0);
                (true, binder, *count)
            }
            None => (false, 0, 0),
        };
        self.record(ProviderLookup {
            method: "refContentProvider".to_string(),
            authority: authority.unwrap_or_default(),
            caller: String::new(),
            hit,
            binder,
            refcount,
        });
        hit
    }

    /// Every recorded transaction, oldest first (at most
    /// [`MAX_PROVIDER_LOOKUPS`]).
    #[must_use]
    pub fn lookups(&self) -> &[ProviderLookup] {
        &self.lookups
    }

    /// How many lookups fell out of the evidence log.
    #[must_use]
    pub fn lookups_dropped(&self) -> u64 {
        self.lookups_dropped
    }
}

/// The `ContentProviderConnection` token handed back in a
/// [`crate::atms`] `ContentProviderHolder`.
///
/// The framework's `ContentProviderHolder.connection` is a real Binder object
/// (`com.android.server.am.ContentProviderConnection extends Binder`); the
/// client never transacts on it, it only writes it back as the identity token of
/// `IActivityManager.removeContentProvider` / `refContentProvider`. ACOS serves
/// it from `ServiceHost` so the holder carries a real handle, and any attempted
/// transaction is the structured unsupported error (the token has no interface).
pub struct ProviderConnectionService;

impl ServiceObject for ProviderConnectionService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        _args: &[Value],
    ) -> Result<CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: format!("{} on a ContentProviderConnection token", method.name),
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
