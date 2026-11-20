//! Activity-manager service side: guest-initiated `startActivity`,
//! `startService`/`stopService`/`bindService`/`unbindService`,
//! `registerReceiver`/`unregisterReceiver`/`broadcastIntent`, and
//! `getContentProvider`/`publishContentProviders` acquisition.
//!
//! This is the state machine behind `android.app.IActivityManager` /
//! `android.app.IActivityTaskManager` for the methods the dogfood fixtures
//! (`service-provider`, `two-activity`) reach. It is intentionally VM-free:
//! like every other `acos-services` module it consumes the decoded
//! [`acos_aidl`] argument list and records the *typed* intent facts, pending
//! launches and pending binder deliveries. `acos-runtime` materialises the real
//! framework objects (`Intent`, `ClientTransaction`, `IServiceConnection`
//! deliveries) from those facts.
//!
//! ## Intent facts
//!
//! A real AIDL `Intent` argument is written with `Intent.writeToParcel`. Rather
//! than duplicate that (large, versioned) layout here, `acos-runtime::ams_java`
//! reads the real `android.content.Intent` object through the Java bridge and
//! reduces it to the typed [`IntentFacts`] this module consumes. The reduction
//! is lossless for the fields the fixtures use (action, data, package,
//! explicit component, categories, string/int/bool extras); an intent option
//! this module cannot honour is a structured error, never a silent success.
//!
//! ## Never fake
//!
//! * An implicit intent that matches zero or more than one declared component is
//!   a structured `SERVICE_ILLEGAL_STATE`, never a coin flip.
//! * `stopService` on a service that was never started returns `false`.
//! * `getContentProvider` for an authority with no published provider returns
//!   `null` (exactly what a real `ContentResolver.acquireProvider` sees).
//! * Any unsupported transaction (flag, launch mode, provider method) raises
//!   `UNSUPPORTED_ANDROID_API`.

use std::collections::{BTreeMap, BTreeSet};

use acos_aidl::MethodInfo;

use crate::error::{Result, ServiceError};
use crate::host::CallResult;
use crate::value::{Bundle, Parcelable, Value};

// ---------------------------------------------------------------------------
// Intent facts
// ---------------------------------------------------------------------------

/// The field indices of the facts AMS reads out of an `android.content.Intent`.
///
/// Since W35 the `Intent` parcelable is the **real** wire shape
/// (`parcelable::read_intent`/`write_intent`, `Intent.writeToParcel` order), so
/// these are that codec's indices — not a reduced six-slot prefix:
/// `action, data, type, identifier, flags, package, component, sourceBounds,
/// categories, selector, clipData, contentUserHint, extras, originalIntent`.
/// A caller that reduces an Intent by hand must produce the same 14 fields
/// (`to_parcelable` does), or `startActivity` resolves the wrong slot.
pub const INTENT_FIELD_ACTION: usize = crate::parcelable::INTENT_ACTION;
/// `Intent.mData` (rendered as a string; a non-null Uri is refused by the codec).
pub const INTENT_FIELD_DATA: usize = crate::parcelable::INTENT_DATA;
/// `Intent.mType` (the MIME type).
pub const INTENT_FIELD_TYPE: usize = crate::parcelable::INTENT_TYPE;
/// `Intent.mIdentifier`.
pub const INTENT_FIELD_IDENTIFIER: usize = crate::parcelable::INTENT_IDENTIFIER;
/// `Intent.mFlags`.
pub const INTENT_FIELD_FLAGS: usize = crate::parcelable::INTENT_FLAGS;
/// `Intent.mPackage`.
pub const INTENT_FIELD_PACKAGE: usize = crate::parcelable::INTENT_PACKAGE;
/// `Intent.mComponent` (`android.content.ComponentName`, 2 String16 fields).
pub const INTENT_FIELD_COMPONENT: usize = crate::parcelable::INTENT_COMPONENT;
/// `Intent.mCategories` (a String list).
pub const INTENT_FIELD_CATEGORIES: usize = crate::parcelable::INTENT_CATEGORIES;
/// `Intent.mExtras` (`Bundle`).
pub const INTENT_FIELD_EXTRAS: usize = crate::parcelable::INTENT_EXTRAS;
/// `Intent.mSourceBounds` (a `Rect` parcelable, or null).
pub const INTENT_FIELD_SOURCE_BOUNDS: usize = crate::parcelable::INTENT_SOURCE_BOUNDS;
/// `Intent.mSelector` (a nested `Intent`, or null).
pub const INTENT_FIELD_SELECTOR: usize = crate::parcelable::INTENT_SELECTOR;
/// `Intent.mClipData` (a `ClipData` parcelable, or null).
pub const INTENT_FIELD_CLIP_DATA: usize = crate::parcelable::INTENT_CLIP_DATA;
/// `Intent.mContentUserHint`.
pub const INTENT_FIELD_CONTENT_USER_HINT: usize = crate::parcelable::INTENT_CONTENT_USER_HINT;

/// The typed intent facts the AMS state machine resolves against.
///
/// These are the fields of the real `android.content.Intent` the session acts
/// on: the resolver needs action/data/type/categories/component/package, and
/// `Intent.fillIn` (PIFILLIN) needs every field the merge copies — including
/// `type`, `identifier`, `flags`, `selector`, `sourceBounds` and `clipData`.
/// A field the codec cannot carry (a non-null `Uri` body) stays a structured
/// refusal in [`crate::parcelable::read_intent`], never a silent `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct IntentFacts {
    /// `Intent.getAction()`.
    pub action: Option<String>,
    /// `Intent.getDataString()`.
    pub data: Option<String>,
    /// `Intent.getType()` (the MIME type). MPROC1: the capture target records
    /// it and the intent-filter match uses it; `None` for an intent without a
    /// type, which is not the same as the wildcard `*/*`.
    pub mime_type: Option<String>,
    /// `Intent.getIdentifier()`. PIFILLIN: `fillIn` copies it when the target
    /// has none (or the caller asked for `FILL_IN_IDENTIFIER`), so the field is
    /// carried, not dropped.
    pub identifier: Option<String>,
    /// `Intent.getFlags()`. MPROC1 records it on a capture; the launch path
    /// takes the flag word from the transaction's own argument (they are the
    /// same value the framework wrote).
    pub flags: i32,
    /// `Intent.getPackage()`.
    pub package: Option<String>,
    /// Explicit component `(package, class)`.
    pub component: Option<(String, String)>,
    /// `Intent.getSourceBounds()` — `(left, top, right, bottom)`.
    pub source_bounds: Option<[i32; 4]>,
    /// `Intent.getCategories()`.
    pub categories: Vec<String>,
    /// `Intent.getSelector()`.
    pub selector: Option<Box<IntentFacts>>,
    /// `Intent.getClipData()`: presence only (the codec carries the parcelable
    /// value but does not interpret it).
    pub clip_data: Option<Parcelable>,
    /// `Intent.getContentUserHint()` (`UserHandle.USER_CURRENT` =-2 when unset).
    pub content_user_hint: i32,
    /// `Intent.getExtras()` (string/int/bool values; other types recorded as
    /// present without a value).
    pub extras: BTreeMap<String, Extra>,
}

/// One intent extra value.
#[derive(Debug, Clone, PartialEq)]
pub enum Extra {
    /// `putExtra(String)`.
    String(String),
    /// `putExtra(int)`.
    Int(i32),
    /// `putExtra(String, short)`: a `VAL_SHORT` bundle value, which
    /// `Bundle.getShort` casts to `java.lang.Short` (`BluetoothDevice`'s
    /// `EXTRA_RSSI` is read with `getShortExtra`).
    Short(i16),
    /// `putExtra(boolean)`.
    Bool(bool),
    /// `putExtra(String[])` — the permission names of a
    /// `REQUEST_PERMISSIONS` intent (`EXTRA_REQUEST_PERMISSIONS_NAMES`).
    Strings(Vec<String>),
    /// `putExtra(int[])` — the grant results a permission dialog returns
    /// (`EXTRA_REQUEST_PERMISSIONS_RESULTS`, PARCEL_FIELD `VAL_INTARRAY`).
    Ints(Vec<i32>),
    /// `putExtra(String, byte[])` (a `VAL_BYTEARRAY` bundle value).
    Bytes(Vec<u8>),
    /// `putExtra(String, Serializable)` of an `Object[]` of `byte[]`: the shape
    /// `SMS_RECEIVED` uses for its `pdus` extra (`Parcel.VAL_OBJECTARRAY`).
    ByteArrays(Vec<Vec<u8>>),
    /// A parcelable extra (a `BluetoothDevice` on `ACTION_FOUND`). The payload
    /// is written with its class name, exactly like `Intent.putExtra(Parcelable)`.
    Parcelable(crate::value::Parcelable),
    /// A value of a type ACOS records but does not interpret.
    Present,
}

impl Default for IntentFacts {
    fn default() -> Self {
        // `Intent()` leaves `mContentUserHint` at `UserHandle.USER_CURRENT`
        // (`-2`), which `Intent.writeToParcel` writes and `fillIn` compares
        // against; a `0` here would claim a user hint the frame never had.
        Self {
            action: None,
            data: None,
            mime_type: None,
            identifier: None,
            flags: 0,
            package: None,
            component: None,
            source_bounds: None,
            categories: Vec::new(),
            selector: None,
            clip_data: None,
            content_user_hint: crate::fill_in::USER_CURRENT,
            extras: BTreeMap::new(),
        }
    }
}

impl IntentFacts {
    /// An explicit-component intent (`new Intent(context, Class)`).
    #[must_use]
    pub fn explicit(package: impl Into<String>, class: impl Into<String>) -> Self {
        Self {
            component: Some((package.into(), class.into())),
            ..Self::default()
        }
    }

    /// An implicit intent with an action.
    #[must_use]
    pub fn action(action: impl Into<String>) -> Self {
        Self {
            action: Some(action.into()),
            ..Self::default()
        }
    }

    /// Add a category.
    #[must_use]
    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.categories.push(category.into());
        self
    }

    /// Add a string extra.
    #[must_use]
    pub fn with_string_extra(mut self, key: &str, value: impl Into<String>) -> Self {
        self.extras
            .insert(key.to_string(), Extra::String(value.into()));
        self
    }

    /// Reduce the typed `android.content.Intent` parcelable produced by
    /// `ams_java`.
    #[must_use]
    pub fn from_parcelable(p: &Parcelable) -> Self {
        let Some(f) = p.fields() else {
            return Self::default();
        };
        let component = f
            .get(INTENT_FIELD_COMPONENT)
            .and_then(Value::as_parcelable)
            .and_then(|c| {
                let package = c.field(0).and_then(Value::as_str)?.to_string();
                let class = c.field(1).and_then(Value::as_str)?.to_string();
                Some((package, class))
            });
        let categories = f
            .get(INTENT_FIELD_CATEGORIES)
            .map(string_list)
            .unwrap_or_default();
        let extras = f
            .get(INTENT_FIELD_EXTRAS)
            .and_then(Value::as_bundle)
            .map(reduce_extras)
            .unwrap_or_default();
        let selector = f
            .get(INTENT_FIELD_SELECTOR)
            .and_then(Value::as_parcelable)
            .filter(|p| !matches!(p, Parcelable::Null))
            .map(|p| Box::new(Self::from_parcelable(p)));
        let clip_data = f
            .get(INTENT_FIELD_CLIP_DATA)
            .and_then(Value::as_parcelable)
            .filter(|p| !matches!(p, Parcelable::Null))
            .cloned();
        let source_bounds = f
            .get(INTENT_FIELD_SOURCE_BOUNDS)
            .and_then(Value::as_parcelable)
            .filter(|p| !matches!(p, Parcelable::Null))
            .and_then(|p| {
                let values = p.fields()?;
                if values.len() != 4 {
                    return None;
                }
                Some([
                    values[0].as_int()?,
                    values[1].as_int()?,
                    values[2].as_int()?,
                    values[3].as_int()?,
                ])
            });
        Self {
            action: f
                .get(INTENT_FIELD_ACTION)
                .and_then(Value::as_str)
                .map(str::to_string),
            data: f
                .get(INTENT_FIELD_DATA)
                .and_then(Value::as_str)
                .map(str::to_string),
            mime_type: f
                .get(INTENT_FIELD_TYPE)
                .and_then(Value::as_str)
                .map(str::to_string),
            identifier: f
                .get(INTENT_FIELD_IDENTIFIER)
                .and_then(Value::as_str)
                .map(str::to_string),
            flags: f
                .get(INTENT_FIELD_FLAGS)
                .and_then(Value::as_int)
                .unwrap_or(0),
            package: f
                .get(INTENT_FIELD_PACKAGE)
                .and_then(Value::as_str)
                .map(str::to_string),
            component,
            source_bounds,
            categories,
            selector,
            clip_data,
            content_user_hint: f
                .get(INTENT_FIELD_CONTENT_USER_HINT)
                .and_then(Value::as_int)
                .unwrap_or(crate::fill_in::USER_CURRENT),
            extras,
        }
    }

    /// Encode the typed facts as the real `android.content.Intent` parcelable
    /// (all [`crate::parcelable::INTENT_FIELDS`] fields, in wire order), which is
    /// what [`from_parcelable`](Self::from_parcelable) reads and what
    /// `parcelable::write_intent` can serialise. Used by tests and by `ams_java`
    /// when it hands facts across.
    #[must_use]
    pub fn to_parcelable(&self) -> Parcelable {
        let component = self.component.as_ref().map(|(package, class)| {
            Parcelable::ordered(
                "android.content.ComponentName",
                vec![
                    Value::Str(Some(package.clone())),
                    Value::Str(Some(class.clone())),
                ],
            )
        });
        let source_bounds = self.source_bounds.map(|[left, top, right, bottom]| {
            Parcelable::ordered(
                "android.graphics.Rect",
                vec![
                    Value::Int(left),
                    Value::Int(top),
                    Value::Int(right),
                    Value::Int(bottom),
                ],
            )
        });
        // `Intent.writeToParcel` writes `mCategories.size()`, so an empty set and
        // a null set are the same bytes — but `fillIn` distinguishes them
        // (`other.mCategories != null`), and `Intent.readFromParcel` answers
        // null for count 0. Writing the empty list as null keeps the round trip
        // faithful to the wire.
        let categories = if self.categories.is_empty() {
            Value::StringList(None)
        } else {
            Value::StringList(Some(self.categories.iter().cloned().map(Some).collect()))
        };
        Parcelable::ordered(
            crate::parcelable::INTENT_CLASS,
            vec![
                Value::Str(self.action.clone()),
                Value::Str(self.data.clone()),
                Value::Str(self.mime_type.clone()),  // type
                Value::Str(self.identifier.clone()), // identifier
                Value::Int(self.flags),              // flags
                Value::Str(self.package.clone()),
                Value::Parcelable(component.unwrap_or(Parcelable::Null)),
                Value::Parcelable(source_bounds.unwrap_or(Parcelable::Null)),
                categories,
                Value::Parcelable(
                    self.selector
                        .as_deref()
                        .map(Self::to_parcelable)
                        .unwrap_or(Parcelable::Null),
                ),
                Value::Parcelable(self.clip_data.clone().unwrap_or(Parcelable::Null)),
                Value::Int(self.content_user_hint),
                // An Intent with no extras has a **null** `mExtras` (AOSP only
                // creates the Bundle on the first `putExtra`), and `fillIn`
                // distinguishes null from present for its `contentUserHint`
                // side effect. Writing the empty map as an empty bundle would
                // claim a bundle the original never had.
                Value::Bundle(if self.extras.is_empty() {
                    None
                } else {
                    Some(encode_extras(&self.extras))
                }),
                Value::Parcelable(Parcelable::Null), // originalIntent
            ],
        )
    }

    /// `Intent.fillIn(other, flags)` (AOSP `Intent.java:11530`), over the facts.
    ///
    /// The rules live in [`crate::fill_in::fill_in_value`], the one implementation
    /// of the AOSP method: this converts the facts to the real Intent value, runs
    /// it, and reduces the answer back, so the reduced path cannot drift from the
    /// byte-verified one. Returns the `FILL_IN_*` change mask AOSP returns.
    pub fn fill_in(&mut self, other: &Self, flags: i32) -> i32 {
        let mut target = self.to_parcelable();
        let changes =
            crate::fill_in::fill_in_value(&mut target, &other.to_parcelable(), flags).unwrap_or(0);
        *self = Self::from_parcelable(&target);
        changes
    }
}

fn string_list(value: &Value) -> Vec<String> {
    match value {
        Value::StringArray(Some(items)) | Value::StringList(Some(items)) => {
            items.iter().flatten().cloned().collect()
        }
        Value::List(Some(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

fn reduce_extras(bundle: &Bundle) -> BTreeMap<String, Extra> {
    let mut out = BTreeMap::new();
    for (key, value) in &bundle.entries {
        let extra = match value {
            Value::Str(Some(s)) | Value::Str8(Some(s)) => Extra::String(s.clone()),
            Value::StringArray(Some(items)) => {
                Extra::Strings(items.iter().filter_map(|item| item.clone()).collect())
            }
            Value::IntArray(Some(items)) => Extra::Ints(items.clone()),
            Value::Int(i) => Extra::Int(*i),
            Value::Short(s) => Extra::Short(*s),
            Value::Bool(b) => Extra::Bool(*b),
            Value::ByteArray(Some(bytes)) => Extra::Bytes(bytes.clone()),
            Value::Parcelable(parcelable) => Extra::Parcelable(parcelable.clone()),
            Value::ObjectArray(Some(items))
                if items
                    .iter()
                    .all(|item| matches!(item, Value::ByteArray(Some(_)))) =>
            {
                Extra::ByteArrays(
                    items
                        .iter()
                        .filter_map(|item| match item {
                            Value::ByteArray(Some(bytes)) => Some(bytes.clone()),
                            _ => None,
                        })
                        .collect(),
                )
            }
            _ => Extra::Present,
        };
        out.insert(key.to_string(), extra);
    }
    out
}

fn encode_extras(extras: &BTreeMap<String, Extra>) -> Bundle {
    let mut bundle = Bundle::new();
    for (key, value) in extras {
        match value {
            Extra::String(s) => bundle.put(key, Value::Str(Some(s.clone()))),
            Extra::Int(i) => bundle.put(key, Value::Int(*i)),
            Extra::Short(s) => bundle.put(key, Value::Short(*s)),
            Extra::Bool(b) => bundle.put(key, Value::Bool(*b)),
            Extra::Strings(items) => bundle.put(
                key,
                Value::StringArray(Some(items.iter().cloned().map(Some).collect())),
            ),
            Extra::Ints(items) => bundle.put(key, Value::IntArray(Some(items.clone()))),
            Extra::Bytes(bytes) => bundle.put(key, Value::ByteArray(Some(bytes.clone()))),
            Extra::Parcelable(parcelable) => bundle.put(key, Value::Parcelable(parcelable.clone())),
            Extra::ByteArrays(pdus) => bundle.put(
                key,
                Value::ObjectArray(Some(
                    pdus.iter()
                        .map(|pdu| Value::ByteArray(Some(pdu.clone())))
                        .collect(),
                )),
            ),
            Extra::Present => bundle.put(key, Value::Skipped),
        }
    }
    bundle
}

// ---------------------------------------------------------------------------
// Manifest database and intent resolution
// ---------------------------------------------------------------------------

/// A reduced `<intent-filter>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// `<action android:name>`.
    pub actions: Vec<String>,
    /// `<category android:name>`.
    pub categories: Vec<String>,
    /// `<data android:scheme>`.
    pub schemes: Vec<String>,
    /// `<data android:mimeType>`.
    pub mime_types: Vec<String>,
}

impl Filter {
    /// Receiver matching: unlike activities, an implicit broadcast does not
    /// require `CATEGORY_DEFAULT`.
    #[must_use]
    pub fn matches_receiver(&self, intent: &IntentFacts) -> bool {
        if let Some(action) = &intent.action {
            if !self.actions.iter().any(|a| a == action) {
                return false;
            }
        }
        if !intent
            .categories
            .iter()
            .all(|c| self.categories.iter().any(|f| f == c))
        {
            return false;
        }
        if let Some(scheme) = intent.data.as_deref().and_then(scheme_of) {
            if !self.schemes.is_empty() && !self.schemes.iter().any(|s| s == scheme) {
                return false;
            }
        }
        true
    }

    /// Android's matching rule for the filter subset ACOS models: every intent
    /// action/category/scheme must be present in the filter. An intent with no
    /// categories only matches a filter that declares `DEFAULT`.
    #[must_use]
    pub fn matches(&self, intent: &IntentFacts) -> bool {
        if let Some(action) = &intent.action {
            if !self.actions.iter().any(|a| a == action) {
                return false;
            }
        }
        let categories = if intent.categories.is_empty() {
            vec!["android.intent.category.DEFAULT".to_string()]
        } else {
            intent.categories.clone()
        };
        if !categories
            .iter()
            .all(|c| self.categories.iter().any(|f| f == c))
        {
            return false;
        }
        if let Some(scheme) = intent.data.as_deref().and_then(scheme_of) {
            if !self.schemes.is_empty() && !self.schemes.iter().any(|s| s == scheme) {
                return false;
            }
        }
        true
    }
}

fn scheme_of(data: &str) -> Option<&str> {
    data.split_once(':').map(|(scheme, _)| scheme)
}

/// A declared activity/service/receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredComponent {
    /// Owning package.
    pub package: String,
    /// Fully-qualified class name.
    pub class: String,
    /// `android:exported`.
    pub exported: bool,
    /// `android:launchMode` (`standard`=0, `singleTop`=1, ...).
    pub launch_mode: i32,
    /// Intent filters.
    pub filters: Vec<Filter>,
}

impl DeclaredComponent {
    /// Build a declared component from an `acos-apk` manifest component.
    fn from_apk(package: &str, c: &acos_apk::manifest::Component) -> Self {
        Self {
            package: package.to_string(),
            class: acos_apk::manifest::fq_name(package, &c.name),
            exported: c.exported.unwrap_or(false),
            launch_mode: launch_mode_code(c.launch_mode.as_deref()),
            filters: c
                .intent_filters
                .iter()
                .map(|f| Filter {
                    actions: f.actions.clone(),
                    categories: f.categories.clone(),
                    schemes: f.data.iter().filter_map(|d| d.scheme.clone()).collect(),
                    mime_types: f.data.iter().filter_map(|d| d.mime_type.clone()).collect(),
                })
                .collect(),
        }
    }
}

/// A declared content provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredProvider {
    /// Owning package.
    pub package: String,
    /// Fully-qualified class name.
    pub class: String,
    /// `android:authorities` (ACOS models the single declared authority).
    pub authority: String,
    /// `android:exported`.
    pub exported: bool,
}

/// One installed package's reduced manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageRecord {
    /// Package name.
    pub package: String,
    /// uid.
    pub uid: i32,
    /// Declared activities.
    pub activities: Vec<DeclaredComponent>,
    /// Declared services.
    pub services: Vec<DeclaredComponent>,
    /// Declared receivers.
    pub receivers: Vec<DeclaredComponent>,
    /// Declared providers.
    pub providers: Vec<DeclaredProvider>,
}

impl PackageRecord {
    /// Build from an `acos-apk` manifest.
    #[must_use]
    pub fn from_manifest(manifest: &acos_apk::manifest::Manifest, uid: i32) -> Self {
        Self {
            package: manifest.package.clone(),
            uid,
            activities: manifest
                .activities
                .iter()
                .map(|c| DeclaredComponent::from_apk(&manifest.package, c))
                .collect(),
            services: manifest
                .services
                .iter()
                .map(|c| DeclaredComponent::from_apk(&manifest.package, c))
                .collect(),
            receivers: manifest
                .receivers
                .iter()
                .map(|c| DeclaredComponent::from_apk(&manifest.package, c))
                .collect(),
            providers: manifest
                .providers
                .iter()
                .map(|c| DeclaredProvider {
                    package: manifest.package.clone(),
                    class: acos_apk::manifest::fq_name(&manifest.package, &c.name),
                    // `android:authorities` is the real key; the class name is
                    // only a last-resort fallback for a malformed manifest.
                    authority: c
                        .authorities
                        .first()
                        .cloned()
                        .unwrap_or_else(|| c.name.clone()),
                    exported: c.exported.unwrap_or(false),
                })
                .collect(),
        }
    }
}

/// The installed-package manifest table.
#[derive(Debug, Clone, Default)]
pub struct ManifestDb {
    /// Installed packages.
    pub packages: Vec<PackageRecord>,
}

impl ManifestDb {
    /// An empty database.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a package from an APK path.
    pub fn add_apk(&mut self, path: impl AsRef<std::path::Path>, uid: i32) -> Result<()> {
        let path = path.as_ref();
        let apk = acos_apk::Apk::open(path)
            .map_err(|e| ServiceError::Internal(format!("open {}: {e}", path.display())))?;
        let manifest = apk
            .manifest()
            .map_err(|e| ServiceError::Internal(format!("manifest {}: {e}", path.display())))?;
        self.add_manifest(&manifest, uid);
        Ok(())
    }

    /// Add a parsed manifest.
    pub fn add_manifest(&mut self, manifest: &acos_apk::manifest::Manifest, uid: i32) {
        if let Some(existing) = self
            .packages
            .iter_mut()
            .find(|p| p.package == manifest.package)
        {
            *existing = PackageRecord::from_manifest(manifest, uid);
        } else {
            self.packages
                .push(PackageRecord::from_manifest(manifest, uid));
        }
    }

    /// The record for a package.
    #[must_use]
    pub fn package(&self, name: &str) -> Option<&PackageRecord> {
        self.packages.iter().find(|p| p.package == name)
    }

    /// Resolve an activity via explicit component or implicit filter.
    pub fn resolve_activity(&self, intent: &IntentFacts) -> Result<DeclaredComponent> {
        self.resolve_component(intent, ComponentKind::Activity)
    }

    /// Resolve a service via explicit component or implicit filter.
    pub fn resolve_service(&self, intent: &IntentFacts) -> Result<DeclaredComponent> {
        self.resolve_component(intent, ComponentKind::Service)
    }

    /// Every declared activity matching `intent`, across every installed
    /// package. Unlike [`ManifestDb::resolve_activity`] this does not pick one
    /// and does not fail on zero or many: `IPackageManager.queryIntentActivities`
    /// returns the full list (empty is a legitimate "no match").
    #[must_use]
    pub fn matching_activities(&self, intent: &IntentFacts) -> Vec<DeclaredComponent> {
        if let Some((package, class)) = &intent.component {
            return self
                .component_by_class(package, class, ComponentKind::Activity)
                .into_iter()
                .collect();
        }
        let mut out = Vec::new();
        for package in &self.packages {
            if let Some(filter_pkg) = &intent.package {
                if filter_pkg != &package.package {
                    continue;
                }
            }
            for component in &package.activities {
                if self.component_matches(component, intent) {
                    out.push(component.clone());
                }
            }
        }
        out
    }

    /// All manifest receivers matching `intent`.
    #[must_use]
    pub fn resolve_receivers(&self, intent: &IntentFacts) -> Vec<DeclaredComponent> {
        let mut out = Vec::new();
        for package in &self.packages {
            for receiver in &package.receivers {
                if receiver.filters.iter().any(|f| f.matches_receiver(intent)) {
                    out.push(receiver.clone());
                }
            }
        }
        out
    }

    /// The provider declaring `authority`.
    #[must_use]
    pub fn resolve_provider(&self, authority: &str) -> Option<DeclaredProvider> {
        self.packages
            .iter()
            .flat_map(|p| p.providers.iter())
            .find(|p| p.authority == authority)
            .cloned()
    }

    fn resolve_component(
        &self,
        intent: &IntentFacts,
        kind: ComponentKind,
    ) -> Result<DeclaredComponent> {
        if let Some((package, class)) = &intent.component {
            let component = self
                .component_by_class(package, class, kind)
                .ok_or_else(|| ServiceError::IllegalState {
                    component: "ams",
                    detail: format!(
                        "explicit {} component {package}/{class} is not declared in the manifest",
                        kind.as_str()
                    ),
                })?;
            if !component.exported && component.package != *package {
                return Err(ServiceError::IllegalState {
                    component: "ams",
                    detail: format!("{} {class} is not exported", kind.as_str()),
                });
            }
            return Ok(component);
        }

        let mut matches: Vec<DeclaredComponent> = Vec::new();
        for package in &self.packages {
            if let Some(filter_pkg) = &intent.package {
                if filter_pkg != &package.package {
                    continue;
                }
            }
            for component in package.components(kind) {
                if self.component_matches(component, intent) {
                    matches.push(component.clone());
                }
            }
        }
        match matches.len() {
            0 => Err(ServiceError::IllegalState {
                component: "ams",
                detail: format!("no {} matches action {:?}", kind.as_str(), intent.action),
            }),
            1 => Ok(matches.pop().expect("one match")),
            _ => Err(ServiceError::IllegalState {
                component: "ams",
                detail: format!(
                    "ambiguous {} resolution: {} candidates",
                    kind.as_str(),
                    matches.len()
                ),
            }),
        }
    }

    fn component_matches(&self, component: &DeclaredComponent, intent: &IntentFacts) -> bool {
        component.filters.iter().any(|f| f.matches(intent))
    }

    fn component_by_class(
        &self,
        package: &str,
        class: &str,
        kind: ComponentKind,
    ) -> Option<DeclaredComponent> {
        let record = self.package(package)?;
        record
            .components(kind)
            .iter()
            .find(|c| c.class == class || c.class == format!("{package}.{class}"))
            .cloned()
    }
}

impl PackageRecord {
    fn components(&self, kind: ComponentKind) -> &[DeclaredComponent] {
        match kind {
            ComponentKind::Activity => &self.activities,
            ComponentKind::Service => &self.services,
            ComponentKind::Receiver => &self.receivers,
        }
    }
}

/// Which component table to resolve against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentKind {
    /// `<activity>`.
    Activity,
    /// `<service>`.
    Service,
    /// `<receiver>`.
    Receiver,
}

impl ComponentKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Activity => "activity",
            Self::Service => "service",
            Self::Receiver => "receiver",
        }
    }
}

fn launch_mode_code(mode: Option<&str>) -> i32 {
    match mode {
        Some("singleTop") => 1,
        Some("singleTask") => 2,
        Some("singleInstance") => 3,
        Some("singleInstancePerTask") => 4,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

/// `ActivityManager.START_SUCCESS`.
pub const START_SUCCESS: i32 = 0;
/// `ActivityManager.START_CANCELED`.
pub const START_CANCELED: i32 = 2;
/// `ActivityManager.START_NOT_FOUND`.
pub const START_NOT_FOUND: i32 = 4;

/// The start code for a captured intent.
///
/// `START_SUCCESS` is what the framework would return for
/// `startActivityForResult`; `START_RETURN_INTENT_TO_CALLER` would tell the
/// caller nobody handled it, which is wrong here — the capture target *did*
/// accept the intent (and will answer if a result was scripted), so the app
/// gets the normal "it was handled" path and the reply says `captured`.
const START_SUCCESS_CAPTURED: i32 = 0;

fn capture_start_success(record: &crate::intents::CapturedIntent) -> i32 {
    let _ = record;
    START_SUCCESS_CAPTURED
}
/// `Service.START_NOT_STICKY`.
pub const START_NOT_STICKY: i32 = 2;

/// Task/back-stack launch behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchBehaviour {
    /// Create a new instance in the current task (`standard`).
    NewInstance,
    /// Reuse the top instance and deliver `onNewIntent` (`singleTop`).
    ReuseTop,
    /// Finish everything above and reuse (`singleTask`).
    FinishAboveAndReuse,
    /// Create the activity in a new task (`singleInstance` / `NEW_TASK`).
    NewTask,
}

/// One activity record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEntry {
    /// Binder token.
    pub token: u64,
    /// Owning package.
    pub package: String,
    /// Activity class.
    pub class: String,
    /// Task id.
    pub task_id: i32,
    /// `finished`.
    pub finished: bool,
    /// `onNewIntent` delivered (rather than a new instance).
    pub reused: bool,
}

/// One task/back-stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Monotonic task id.
    pub id: i32,
    /// Activity tokens from bottom to top.
    pub activities: Vec<u64>,
}

/// One started/bound service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRecord {
    /// Owning package.
    pub package: String,
    /// Service class.
    pub class: String,
    /// `startService` has run.
    pub started: bool,
    /// Number of live `bindService` connections.
    pub bind_count: u32,
}

/// One `PatternMatcher` element of an `IntentFilter` (a scheme-specific part
/// or a path), reduced to the pair the matcher needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DataPattern {
    /// `PatternMatcher.mPattern`.
    pub pattern: Option<String>,
    /// `PatternMatcher.mType` (`PATTERN_LITERAL` = 0, `PATTERN_PREFIX` = 1,
    /// `PATTERN_SIMPLE_GLOB` = 2, `PATTERN_ADVANCED_GLOB` = 3).
    pub kind: i32,
}

impl DataPattern {
    /// AOSP `PatternMatcher.match`.
    fn matches(&self, value: &str) -> bool {
        let Some(pattern) = self.pattern.as_deref() else {
            return false;
        };
        match self.kind {
            0 => pattern == value,
            1 => value.starts_with(pattern),
            // `PATTERN_SIMPLE_GLOB`: `*` and `.` wildcards, as
            // `PatternMatcher.matchPattern`.
            2 => glob_match(pattern, value),
            // `PATTERN_SUFFIX`.
            4 => value.ends_with(pattern),
            // An advanced glob is a superset this matcher does not model; a
            // filter that uses one must not silently match everything.
            _ => false,
        }
    }
}

/// `IntentFilter$AuthorityEntry`: `mOrigHost`, `mWild`, `mPort` (`-1` = any).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DataAuthority {
    /// The host (`*.example.com` when [`Self::wild`]).
    pub host: Option<String>,
    /// `-1` accepts any port.
    pub port: i32,
    /// The `*.host` form.
    pub wild: bool,
}

impl DataAuthority {
    fn matches(&self, host: &str, port: i32) -> bool {
        let Some(declared) = self.host.as_deref() else {
            return false;
        };
        if self.port != -1 && self.port != port {
            return false;
        }
        if self.wild {
            // `*.example.com` matches any sub-domain but not the domain itself
            // (`IntentFilter.AuthorityEntry.match` compares against
            // `mHost` = `.example.com`).
            let suffix = declared.trim_start_matches('*');
            return host.len() > suffix.len() && host.ends_with(suffix);
        }
        declared == host
    }
}

/// The `IntentFilter` facts a dynamic registration is matched on.
///
/// This is the subset of `IntentFilter` that
/// [`IntentFilter.match`](https://android.googlesource.com/platform/frameworks/base/+/refs/tags/android-14.0.0_r75/core/java/android/content/IntentFilter.java)
/// reads for a broadcast: action, categories, and the data triple
/// (scheme / scheme-specific part / authority+port / path) plus the MIME type
/// list. Priority is recorded but never used for **unordered** delivery: stock
/// orders only ordered broadcasts by it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReceiverFilter {
    /// `IntentFilter.mActions`.
    pub actions: Vec<String>,
    /// `IntentFilter.mCategories`.
    pub categories: Vec<String>,
    /// `IntentFilter.mDataSchemes`.
    pub schemes: Vec<String>,
    /// `IntentFilter.mStaticDataTypes` + `mDataTypes` (the filter's types).
    pub data_types: Vec<String>,
    /// `IntentFilter.mDataSchemeSpecificParts`.
    pub scheme_specific_parts: Vec<DataPattern>,
    /// `IntentFilter.mDataAuthorities`.
    pub authorities: Vec<DataAuthority>,
    /// `IntentFilter.mDataPaths`.
    pub paths: Vec<DataPattern>,
    /// `IntentFilter.mPriority` (recorded; unordered delivery ignores it).
    pub priority: i32,
}

/// A parsed `data` string: the parts `IntentFilter.matchData` compares.
struct IntentData<'a> {
    scheme: &'a str,
    rest: &'a str,
    host: Option<&'a str>,
    port: i32,
    path: Option<&'a str>,
}

fn parse_intent_data(data: &str) -> Option<IntentData<'_>> {
    let (scheme, rest) = data.split_once(':')?;
    let (host, port, path) = if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        let authority = &after[..end];
        let path = if end < after.len() {
            Some(&after[end..])
        } else {
            None
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => {
                (host, port.parse::<i32>().unwrap_or(-1))
            }
            _ => (authority, -1),
        };
        (Some(host), port, path)
    } else {
        (None, -1, None)
    };
    Some(IntentData {
        scheme,
        rest,
        host,
        port,
        path,
    })
}

impl ReceiverFilter {
    /// `IntentFilter.match(action, type, scheme, data)` — the AOSP answer as a
    /// boolean. Categories are the caller's subset rule (`matchCategories`).
    #[must_use]
    pub fn matches(&self, intent: &IntentFacts) -> bool {
        // `IntentFilter.match`: `if (action != null && !matchAction(action))`.
        // An intent with no action matches any filter.
        if let Some(action) = &intent.action {
            if !self.actions.iter().any(|a| a == action) {
                return false;
            }
        }
        // `matchCategories`: every intent category must be declared.
        if !intent
            .categories
            .iter()
            .all(|c| self.categories.iter().any(|f| f == c))
        {
            return false;
        }
        self.matches_data(intent)
    }

    /// `IntentFilter.matchData`. The filter has no data constraints only when
    /// it declares no schemes and no types; then it matches an intent that
    /// carries neither a type nor a data URI.
    fn matches_data(&self, intent: &IntentFacts) -> bool {
        let parsed = intent.data.as_deref().and_then(parse_intent_data);
        let type_ = intent.mime_type.as_deref();
        if self.schemes.is_empty() && self.data_types.is_empty() {
            return type_.is_none() && parsed.is_none();
        }
        if !self.schemes.is_empty() {
            let Some(data) = parsed.as_ref() else {
                return false;
            };
            if !self.schemes.iter().any(|s| s == data.scheme) {
                return false;
            }
            if !self.scheme_specific_parts.is_empty()
                && !self
                    .scheme_specific_parts
                    .iter()
                    .any(|p| p.matches(data.rest))
            {
                return false;
            }
            if !self.authorities.is_empty() {
                let Some(host) = data.host else {
                    return false;
                };
                if !self
                    .authorities
                    .iter()
                    .any(|a| a.matches(host, data.port))
                {
                    return false;
                }
            }
            if !self.paths.is_empty() {
                let Some(path) = data.path else {
                    return false;
                };
                if !self.paths.iter().any(|p| p.matches(path)) {
                    return false;
                }
            }
        } else if parsed.is_some() {
            // A filter that declares types but no scheme does not match a
            // `data:` URI (`IntentFilter`'s "types without schemes" case).
            return false;
        }
        if self.data_types.is_empty() {
            // `IntentFilter`: with no types declared, an intent that carries a
            // type is a data mismatch.
            return type_.is_none();
        }
        type_.is_some_and(|t| self.data_types.iter().any(|d| mime_matches(d, t)))
    }
}

/// `IntentFilter.findMatchingType` for one filter type: `*/*` matches
/// anything, `x/*` matches every subtype of `x`.
fn mime_matches(filter: &str, intent: &str) -> bool {
    if filter == intent {
        return true;
    }
    let Some((filter_major, filter_minor)) = filter.split_once('/') else {
        return false;
    };
    if filter_major == "*" && filter_minor == "*" {
        return true;
    }
    let Some((intent_major, _)) = intent.split_once('/') else {
        return false;
    };
    filter_minor == "*" && filter_major == intent_major
}

/// `PatternMatcher.PATTERN_SIMPLE_GLOB`, exactly as
/// `PatternMatcher.matchPattern` reads it: `.` matches any one character,
/// `X*` matches zero or more occurrences of `X` (the character immediately
/// before the `*`), and `\\` escapes the next character. The whole string must
/// match.
fn glob_match(pattern: &str, value: &str) -> bool {
    fn walk(p: &[char], v: &[char]) -> bool {
        let Some(&first) = p.first() else {
            return v.is_empty();
        };
        let (token, after) = if first == '\\' {
            match p.get(1) {
                Some(&escaped) => (escaped, &p[2..]),
                None => return false,
            }
        } else if first == '*' {
            // A `*` never stands alone in `PATTERN_SIMPLE_GLOB`; a pattern that
            // starts with one matches nothing (`matchPattern`'s own refusal).
            return false;
        } else {
            (first, &p[1..])
        };
        if after.first() == Some(&'*') {
            let rest = &after[1..];
            if token == '.' {
                // `.*` is any run; the rest of the pattern decides how much.
                return (0..=v.len()).any(|skip| walk(rest, &v[skip..]));
            }
            let mut same = 0;
            while same < v.len() && v[same] == token {
                same += 1;
            }
            return (0..=same).any(|n| walk(rest, &v[n..]));
        }
        if token == '.' {
            return !v.is_empty() && walk(after, &v[1..]);
        }
        v.first() == Some(&token) && walk(after, &v[1..])
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    walk(&pattern, &value)
}

/// One registered `IIntentReceiver`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverRecord {
    /// `IIntentReceiver` binder.
    pub receiver: u64,
    /// Owning package (empty when the registering caller did not name one).
    pub package: String,
    /// The `IntentFilter` facts the broadcast is matched on.
    pub filter: ReceiverFilter,
    /// `registerReceiver` flags.
    pub flags: i32,
}

/// One published content provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRecord {
    /// `android:authorities`.
    pub authority: String,
    /// Owning package.
    pub package: String,
    /// Provider class.
    pub class: String,
    /// The `IContentProvider` binder the app published (0 when not published).
    pub binder: u64,
}

/// Who asked for a recorded launch (MERGEFIX2).
///
/// The two sides of ADR-0029's launch seam own **different** launches, and a
/// launch must be delivered exactly once:
///
/// * [`LaunchOrigin::Guest`] — the guest's own `startActivity`. The guest's
///   start-activity guard (`LaunchMain`) materialises it in the process that
///   asked (`start-activity-launched`) and adopts the token it minted, exactly
///   as `ActivityThread` would after `scheduleTransaction`; a **primary**
///   process's activity token is a guest `Binder` and is reported back on
///   `activityResumed`/`finishActivity`, so the host must not mint a second one.
/// * [`LaunchOrigin::Host`] — `system_server` as the client: a module firing an
///   `IIntentSender` (the notification shade's row tap), the permission
///   dialog's start, a scripted result. No guest `startActivity` is behind it,
///   so only the host can deliver it (`Peer::take_host_transactions`).
///
/// A **spawned secondary** process is `ACOSLAUNCH_HOST_SCHEDULES` and waits for
/// the host's transaction for every origin (MPROC7), which is why the filter is
/// per origin *and* per process (see [`AmsState::pending_launch_for_tag`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchOrigin {
    /// The guest asked (`IActivityTaskManager.startActivity…`).
    Guest,
    /// The host asked (a module's `IIntentSender`, the dialog's start).
    Host,
}

/// A `startActivity` the runtime must turn into a real `ClientTransaction`.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingLaunch {
    /// Activity token (`ActivityRecord.token`).
    pub token: u64,
    /// Owning package.
    pub package: String,
    /// Activity class.
    pub class: String,
    /// The resolved intent.
    pub intent: IntentFacts,
    /// Task the activity belongs to.
    pub task_id: i32,
    /// `startActivityForResult` result receiver (`0` when none).
    pub result_to: u64,
    /// `startActivityForResult` request code.
    pub request_code: i32,
    /// The AIDL `resultWho` string (`Activity.requestPermissions` passes
    /// `"@android:requestPermissions:"`; a normal launch passes null), which
    /// `Activity.dispatchActivityResult` uses to route the result.
    pub result_who: Option<String>,
    /// Who asked for this launch (MERGEFIX2): it decides which side delivers it.
    pub origin: LaunchOrigin,
}

/// A `bindService` the runtime must deliver to the real `IServiceConnection`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBind {
    /// The `IServiceConnection` binder.
    pub connection: u64,
    /// Owning package.
    pub package: String,
    /// Service class.
    pub class: String,
    /// The service binder (a real `Binder`; `0` when the service returned null).
    pub binder: u64,
}

/// A process the launcher must start (MPROC4 step 3b).
///
/// On a device `ActivityManagerService.startProcessLocked` asks the zygote for a
/// new process as soon as a component resolves to a package that has none. ACOS
/// has no system process, so the resolution path records the request here and
/// the launcher (which owns the processes) drains it and spawns the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpawnRequest {
    /// Package that needs a process (`ApplicationInfo.packageName`).
    pub package: String,
    /// The component that triggered it (`package/class`, class may be empty).
    pub component: String,
    /// Why the process is needed: `startActivity`, `startService`,
    /// `bindService`, `getContentProvider`.
    pub reason: String,
}

/// A broadcast the runtime must deliver to an `IIntentReceiver`.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingReceiver {
    /// `IIntentReceiver` binder.
    pub receiver: u64,
    /// The broadcast intent.
    pub intent: IntentFacts,
    /// Ordered-broadcast result code.
    pub result_code: i32,
    /// Ordered-broadcast result data.
    pub result_data: Option<String>,
    /// `true` when this receiver's delivery is ordered.
    pub ordered: bool,
}

impl PendingReceiver {
    /// The `IIntentReceiver.performReceive` delivery for one queued match.
    ///
    /// This is the ART host's side of a broadcast: the `Intent` is written with
    /// the real `android.content.Intent.writeToParcel` layout
    /// ([`crate::parcelable::write_intent`]), the `Bundle extras` parameter is
    /// the null typed-object (`0`), and the transaction is
    /// `IIntentReceiver.performReceive` (code 1, oneway). A host that receives
    /// this frame hands it to the app's `ReceiverDispatcher`, exactly as
    /// `system_server` does.
    pub fn receiver_delivery(&self) -> Result<crate::host::PendingDelivery> {
        let mut data = acos_parcel::Parcel::new();
        data.write_interface_token("android.content.IIntentReceiver")
            .map_err(ServiceError::Parcel)?;
        // `performReceive(in Intent intent, ...)`: AIDL writes a parcelable as
        // `writeTypedObject` (presence word + `writeToParcel` body, no class
        // name).
        data.write_int32(1);
        crate::parcelable::write_intent(&mut data, &self.intent.to_parcelable())?;
        data.write_int32(self.result_code);
        data.write_string16_opt(self.result_data.as_deref())
            .map_err(ServiceError::Parcel)?;
        data.write_int32(0); // extras: writeTypedObject(null)
        data.write_int32(i32::from(self.ordered));
        data.write_int32(0); // sticky
        data.write_int32(0); // sendingUser (Process.SYSTEM_UID)
        Ok(crate::host::PendingDelivery {
            target: acos_parcel::BinderId(self.receiver),
            interface: "android.content.IIntentReceiver".to_string(),
            code: 1,
            method: "performReceive".to_string(),
            data,
        })
    }
}

/// `Intent.FLAG_RECEIVER_REGISTERED_ONLY`: only dynamically registered
/// receivers hear the broadcast; no manifest `<receiver>` is instantiated.
pub const FLAG_RECEIVER_REGISTERED_ONLY: i32 = 0x4000_0000;

/// A manifest `<receiver>` a broadcast matched, which the **owning app
/// process** has to instantiate (`ActivityThread.handleReceiver`).
///
/// Stock `BroadcastQueue` copies the broadcast intent, sets the receiver's
/// component on the copy, resolves the package's `ActivityInfo` and asks that
/// process's `IApplicationThread.scheduleReceiver` to run it. ACOS's host has
/// no Java, so the same request is a `PendingDelivery`; a match with no running
/// process in its package is reported, never fabricated (a device would start
/// the process, which is the launcher's decision, not this host's).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingManifestReceiver {
    /// Owning package (`ActivityInfo.packageName`).
    pub package: String,
    /// Fully-qualified receiver class (`ActivityInfo.name`).
    pub class: String,
    /// The broadcast intent (the host sets the component on a copy).
    pub intent: IntentFacts,
    /// `BroadcastReceiver.getSentFromPackage()`'s answer.
    pub sending_package: String,
}

/// The AMS state machine.
#[derive(Debug, Default)]
pub struct AmsState {
    /// Installed manifests.
    pub manifests: ManifestDb,
    /// Tasks (back stacks).
    pub tasks: Vec<Task>,
    /// Activity records.
    pub activities: Vec<ActivityEntry>,
    /// Started/bound services, keyed by `package/class`.
    pub services: BTreeMap<String, ServiceRecord>,
    /// Dynamically registered receivers.
    pub receivers: Vec<ReceiverRecord>,
    /// Published providers, keyed by authority.
    pub providers: BTreeMap<String, ProviderRecord>,
    /// Pending launches for the runtime to materialise.
    pub pending_launches: Vec<PendingLaunch>,
    /// Pending `IServiceConnection.connected` deliveries.
    pub pending_binds: Vec<PendingBind>,
    /// Pending `IIntentReceiver.performReceive` deliveries.
    pub pending_receivers: Vec<PendingReceiver>,
    /// Manifest receivers that matched: the owning process instantiates the
    /// class (`IApplicationThread.scheduleReceiver`), it is not an
    /// `IIntentReceiver` delivery.
    pub pending_manifest_receivers: Vec<PendingManifestReceiver>,
    /// The session's one runtime-permission table (MODNOTIF2). Only the
    /// permission-controller request path in [`dispatch`] reads it; `None` in a
    /// bare unit-test state, where a request resolves as unsupported.
    pub runtime_permissions:
        Option<std::sync::Arc<std::sync::Mutex<crate::permissions::PermissionStore>>>,
    /// Next task id.
    next_task_id: i32,
    /// Next activity token.
    next_token: u64,
    /// Last `onNewIntent` records (evidence).
    pub new_intents: Vec<(u64, IntentFacts)>,
    /// MPROC1: the session's intent **capture target**. `Some` in a session
    /// (set by the standard registry); `None` in a bare state machine, where an
    /// unresolvable intent stays the structured `START_NOT_FOUND`.
    pub intents: Option<std::sync::Arc<std::sync::Mutex<crate::intents::IntentState>>>,
    /// W53/WV3: `IActivityManager.addPackageDependency` records, i.e. the
    /// packages the calling guest process declared it depends on. On a device
    /// this is `ProcessRecord.pkgDeps` — the set AMS consults when a package is
    /// updated or removed, which is exactly why `WebViewFactory` registers
    /// `com.android.webview` before it reads that package's `PackageInfo` ("so
    /// that we will be killed if the package info goes out-of-date",
    /// `WebViewFactory.java:459`). ACOS has one guest process per session and
    /// does not uninstall packages while it runs, so the set is session state:
    /// recorded, never fabricated, and readable as evidence. Sorted, so the
    /// recorded order is deterministic.
    pub package_dependencies: BTreeSet<String>,
    /// `PendingIntent` senders: the `mIntentSenderRecords` map, addressed by the
    /// pool of `IIntentSender` binders `registry::install` registers
    /// (MODNOTIF4).
    pub intent_senders: crate::pending_intent::SenderTable,
    /// MPROC7: `onNewIntent` deliveries the host still owes a process
    /// (`FLAG_ACTIVITY_SINGLE_TOP` to an activity that is already the top).
    pub pending_new_intents: Vec<(u64, IntentFacts)>,
    /// MPROC4 step 3b: components that resolved to a package the service layer
    /// cannot run by itself. The launcher drains them and spawns the guest; a
    /// request names only what was resolved, never a pid, a tag or a command
    /// line (those belong to the launcher).
    pub spawn_requests: Vec<ProcessSpawnRequest>,
    /// MPROC8: the guest-initiated cross-process bind table (AOSP
    /// `ActiveServices`). Empty and inert until the session has real app
    /// processes, which the launcher states
    /// ([`crate::host::ServiceHost::set_host_side_services`]).
    pub host_services: crate::service_bind::HostServices,
}

/// Ceiling on queued spawn requests (a bounded queue, never unbounded growth).
pub const MAX_SPAWN_REQUESTS: usize = 64;

impl AmsState {
    /// New empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_task_id: 1,
            next_token: 0x1000_0000,
            runtime_permissions: None,
            ..Self::default()
        }
    }

    /// Start (or reuse) an activity and queue the launch transaction.
    ///
    /// `flags` is the `Intent` flag word; only `FLAG_ACTIVITY_NEW_TASK` (`0x1000_0000`)
    /// and `FLAG_ACTIVITY_CLEAR_TOP` (`0x0400_0000`) are honoured. Any other
    /// launch-affecting flag is a structured error (never ignored).
    pub fn start_activity(
        &mut self,
        intent: &IntentFacts,
        flags: i32,
        result_to: u64,
        request_code: i32,
    ) -> Result<i32> {
        self.start_activity_with_who(intent, flags, result_to, request_code, None)
    }

    /// [`Self::start_activity`] for a launch **the host asked for**
    /// (`system_server` as the client: a module firing an `IIntentSender`, the
    /// permission dialog's start).
    ///
    /// The guest never sees this `startActivity` call, so it cannot materialise
    /// the launch itself; the peer delivers it
    /// ([`PendingLaunch::origin`], [`Self::pending_launch_for_tag`]).
    pub fn start_activity_from_host(
        &mut self,
        intent: &IntentFacts,
        flags: i32,
        result_to: u64,
        request_code: i32,
    ) -> Result<i32> {
        self.start_activity_origin(intent, flags, result_to, request_code, None, LaunchOrigin::Host)
    }

    /// [`Self::start_activity`] with the AIDL `resultWho` string.
    ///
    /// `Activity.requestPermissions` starts the controller with
    /// `startActivityForResult(REQUEST_PERMISSIONS_WHO_PREFIX, …)`, so the
    /// caller's result record carries that string as `mResultWho`; the
    /// `ActivityResultItem` the host builds must hand it back or
    /// `Activity.dispatchActivityResult` never calls
    /// `onRequestPermissionsResult`.
    pub fn start_activity_with_who(
        &mut self,
        intent: &IntentFacts,
        flags: i32,
        result_to: u64,
        request_code: i32,
        result_who: Option<String>,
    ) -> Result<i32> {
        self.start_activity_origin(
            intent,
            flags,
            result_to,
            request_code,
            result_who,
            LaunchOrigin::Guest,
        )
    }

    /// [`Self::start_activity_with_who`] with the fact of **who** asked.
    #[allow(clippy::too_many_arguments)]
    pub fn start_activity_origin(
        &mut self,
        intent: &IntentFacts,
        flags: i32,
        result_to: u64,
        request_code: i32,
        result_who: Option<String>,
        origin: LaunchOrigin,
    ) -> Result<i32> {
        // `Instrumentation.execStartActivity` passes `0` as the AIDL
        // `startFlags` argument; the flags the caller set live **in the
        // `Intent`** (`intent.getFlags()`), and stock `ActivityStarter` ORs the
        // two (`mLaunchFlags`). Reading only the argument made
        // `FLAG_ACTIVITY_SINGLE_TOP` invisible (MPROC7: B's `startActivity` with
        // the flag arrived as a second launch instead of `onNewIntent`).
        let flags = flags | intent.flags;
        const NEW_TASK: i32 = 0x1000_0000;
        const CLEAR_TOP: i32 = 0x0400_0000;
        /// `Intent.FLAG_ACTIVITY_SINGLE_TOP` (MPROC7).
        const SINGLE_TOP: i32 = 0x2000_0000;
        /// `Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED`: AOSP uses it inside
        /// `ActivityStarter` only when an existing task is reused for a **new
        /// root** (the task is reset to its root activity before the start,
        /// `Task.resetTaskIfNeeded`). ACOS keeps one task per process and a
        /// launch is always that task's root, so there is nothing to reset:
        /// accepting the flag is the faithful read, not a silently ignored
        /// semantic difference — NEW_TASK already selects the same behaviour.
        /// Gaten's `LauncherActivity` starts `MainActivity` with
        /// `NEW_TASK|RESET_TASK_IF_NEEDED` (`0x10200000`), which is why the
        /// strict set below has to name it.
        const RESET_TASK_IF_NEEDED: i32 = 0x0020_0000;
        const HONOURED: i32 = NEW_TASK | CLEAR_TOP | SINGLE_TOP | RESET_TASK_IF_NEEDED;
        if flags & !HONOURED != 0 {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: "android.app.IActivityManager".into(),
                code: 14,
                method: format!("startActivity flags {flags:#x}"),
            });
        }
        let component = match self.manifests.resolve_activity(intent) {
            Ok(component) => component,
            Err(ServiceError::IllegalState { .. }) => {
                // MPROC1: nothing installed handles this intent. On a phone the
                // share sheet / browser / picker would; the session's capture
                // target stands in for it (no process, no fabricated app) and
                // `START_SUCCESS` is the honest answer because the intent was
                // accepted and its payload recorded. Anything the capture
                // target does not stand in for stays `START_NOT_FOUND`.
                let captured = self.intents.as_ref().and_then(|state| {
                    state.lock().ok().and_then(|mut state| {
                        state.capture(
                            intent,
                            flags,
                            request_code,
                            // AOSP's `Instrumentation.execStartActivity` passes
                            // requestCode `-1` for a plain `startActivity`, and
                            // `>= 0` only when the caller waits for a result
                            // (`startActivityForResult`). That — not the result
                            // token, which is the activity token either way —
                            // is what "waiting for a result" means.
                            request_code >= 0,
                            crate::intents::now_ms(),
                        )
                    })
                });
                return match captured {
                    Some(record) => Ok(capture_start_success(&record)),
                    None => Err(ServiceError::IllegalState {
                        component: "atms",
                        detail: format!(
                            "no installed package handles action {:?} (and it is not an intent \
                             the capture target stands in for)",
                            intent.action.as_deref().unwrap_or("")
                        ),
                    }),
                };
            }
            Err(other) => return Err(other),
        };
        // AOSP's `ActivityStarter` resolves an implicit intent and hands the
        // launch on with the component set (`intent.setComponent(aInfo
        // .getComponentName())`); `ActivityThread.performLaunchActivity` reads
        // `r.intent.getComponent()` and crashes on null. SYSUI1: the permission
        // request is an implicit action+package intent, so the resolution has to
        // land on the recorded launch exactly as it does on a device.
        let intent = if intent.component.is_none() {
            let mut resolved = intent.clone();
            resolved.component = Some((component.package.clone(), component.class.clone()));
            resolved
        } else {
            intent.clone()
        };
        let intent = &intent;
        let behaviour = if flags & NEW_TASK != 0 || component.launch_mode == 3 {
            LaunchBehaviour::NewTask
        } else {
            match component.launch_mode {
                1 => LaunchBehaviour::ReuseTop,
                2 => LaunchBehaviour::FinishAboveAndReuse,
                // `FLAG_ACTIVITY_SINGLE_TOP` is the per-call spelling of
                // `launchMode="singleTop"` (AOSP `ActivityStarter` folds the flag
                // into the launch flags): a top instance is reused and gets
                // `NewIntentItem`, never a second instance.
                _ if flags & SINGLE_TOP != 0 => LaunchBehaviour::ReuseTop,
                _ => LaunchBehaviour::NewInstance,
            }
        };
        if flags & CLEAR_TOP != 0 && behaviour == LaunchBehaviour::NewInstance {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: "android.app.IActivityManager".into(),
                code: 14,
                method: "CLEAR_TOP without a matching instance".into(),
            });
        }

        let task_id = self.task_for(&component, behaviour);
        let top = self
            .tasks
            .iter()
            .find(|t| t.id == task_id)
            .and_then(|t| t.activities.last().copied());
        let top_same = top
            .and_then(|token| self.activities.iter().find(|a| a.token == token))
            .is_some_and(|a| a.package == component.package && a.class == component.class);

        if matches!(
            behaviour,
            LaunchBehaviour::ReuseTop | LaunchBehaviour::FinishAboveAndReuse
        ) && top_same
        {
            let token = top.expect("top_same implies a top token");
            if behaviour == LaunchBehaviour::ReuseTop {
                self.new_intents.push((token, intent.clone()));
                self.pending_new_intents.push((token, intent.clone()));
                return Ok(START_SUCCESS);
            }
            // `singleTask`: finish everything above the existing instance.
            let above = self.clear_above(task_id, token);
            for finished in above {
                if let Some(a) = self.activities.iter_mut().find(|a| a.token == finished) {
                    a.finished = true;
                }
            }
            self.new_intents.push((token, intent.clone()));
            return Ok(START_SUCCESS);
        }

        let token = self.next_token;
        self.next_token += 1;
        self.activities.push(ActivityEntry {
            token,
            package: component.package.clone(),
            class: component.class.clone(),
            task_id,
            finished: false,
            reused: false,
        });
        self.task_mut(task_id).activities.push(token);
        // MPROC4 step 3b: the activity lives in `component.package`'s process.
        // Record the need here; the launcher decides whether that process
        // already exists (it owns the processes and their tags).
        self.request_process(&component.package, &component.class, "startActivity");
        self.pending_launches.push(PendingLaunch {
            token,
            package: component.package.clone(),
            class: component.class.clone(),
            intent: intent.clone(),
            task_id,
            result_to,
            request_code,
            result_who,
            origin,
        });
        Ok(START_SUCCESS)
    }

    /// True when the framework's permission request can be handed to the guest
    /// system app (SYSUI1).
    ///
    /// Three facts, all from real state: the session's runtime-permission table
    /// exists, the `permissions` module has dialogs on (`system_ui: dialogs`, the
    /// `real` profile's default), and an installed package answers
    /// `ACTION_REQUEST_PERMISSIONS`. When any is missing the caller keeps the
    /// structured `PERMISSION_UI_UNAVAILABLE` — the honest "the controller is
    /// not here" answer of MODNOTIF2, not a silent grant.
    #[must_use]
    pub fn permission_dialog_available(&self, intent: &IntentFacts) -> bool {
        let Some(store) = self.runtime_permissions.as_ref() else {
            return false;
        };
        if !store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .dialogs_enabled()
        {
            return false;
        }
        self.manifests.resolve_activity(intent).is_ok()
    }

    /// Record the delivered request in the store (evidence the dialog was
    /// shown, exactly like `PackageInstaller`'s request log).
    pub fn record_permission_request(&mut self, request: &crate::permissions::PermissionsRequest) {
        if let Some(store) = self.runtime_permissions.as_ref() {
            store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_request(&request.permissions, &request.package);
        }
    }

    /// The launch intent the controller receives: the framework's own request
    /// intent plus the two facts the controller system app cannot read itself
    /// (it may only query *its own* permissions through `checkSelfPermission`).
    ///
    /// * `acos.permission.GRANTED` — permissions already granted, which
    ///   `GrantPermissionsActivity` skips (`allRequestedPermissionsGranted` →
    ///   grant + finish without drawing);
    /// * `acos.permission.FIXED` — denied with `FLAG_PERMISSION_USER_FIXED`
    ///   ("don't ask again"), which `GrantPermissionsActivity` also skips and
    ///   answers denied.
    ///
    /// A real controller calls `IPermissionManager.getPermissionFlags` for these;
    /// ACOS passes them in the launch instead because the guest app cannot reach
    /// the hidden `IPermissionManager` stub. Same facts, one hop earlier.
    #[must_use]
    pub fn permission_dialog_intent(
        &self,
        request: &crate::permissions::PermissionsRequest,
        intent: &IntentFacts,
    ) -> IntentFacts {
        let mut intent = intent.clone();
        if let Some(store) = self.runtime_permissions.as_ref() {
            let store = store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let granted: Vec<String> = request
                .permissions
                .iter()
                .filter(|permission| store.check(permission) == crate::permissions::PERMISSION_GRANTED)
                .cloned()
                .collect();
            let fixed: Vec<String> = request
                .permissions
                .iter()
                .filter(|permission| store.never_ask_again(permission))
                .cloned()
                .collect();
            intent.extras.insert(
                crate::permissions::EXTRA_GRANTED.to_string(),
                Extra::Strings(granted),
            );
            intent.extras.insert(
                crate::permissions::EXTRA_FIXED.to_string(),
                Extra::Strings(fixed),
            );
            intent.extras.insert(
                crate::permissions::EXTRA_CALLING_PACKAGE.to_string(),
                Extra::String(store.package.clone()),
            );
        }
        intent
    }

    /// The structured answer to a `REQUEST_PERMISSIONS` intent (MODNOTIF2).
    ///
    /// Real Android shows `GrantPermissionsActivity` from
    /// `com.android.permissioncontroller`. ACOS does not install that system app
    /// yet, so this answers with the configured state's honest result:
    ///
    /// * all requested permissions already granted -> `state: "granted"`. On a
    ///   phone the controller grants immediately and returns
    ///   `onRequestPermissionsResult` without drawing anything; ACOS cannot
    ///   deliver that result (no `IApplicationThread` activity-result path), so
    ///   it says exactly that instead of answering success and leaving
    ///   `mHasCurrentPermissionsRequest` set forever.
    /// * otherwise -> `state: "denied"`: the dialog is the permission
    ///   controller app's, and it is not built.
    ///
    /// The request is recorded (a refusal is evidence), and nothing is granted.
    pub fn permission_ui_unavailable(
        &mut self,
        request: &crate::permissions::PermissionsRequest,
    ) -> ServiceError {
        let (state, detail) = match self.runtime_permissions.as_ref() {
            Some(store) => {
                let mut store = store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                store.refuse_request(&request.permissions, &request.package);
                if store.all_granted(&request.permissions) {
                    (
                        "granted".to_string(),
                        format!(
                            "every requested permission is already granted ({}); the controller would auto-grant and return, but ACOS cannot deliver onRequestPermissionsResult without the controller system app",
                            request.permissions.join(", ")
                        ),
                    )
                } else {
                    (
                        "denied".to_string(),
                        format!(
                            "{} is not granted and the dialog is the permission controller app's",
                            request
                                .permissions
                                .iter()
                                .filter(|p| store.check(p) != crate::permissions::PERMISSION_GRANTED)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    )
                }
            }
            None => (
                "unknown".to_string(),
                "no runtime-permission table is published in this host".to_string(),
            ),
        };
        ServiceError::PermissionUiUnavailable {
            permissions: request.permissions.clone(),
            state,
            needs: format!(
                "a guest system app {} answering ACTION_REQUEST_PERMISSIONS (GrantPermissionsActivity), built on the multi-process work",
                request.package
            ),
            detail,
        }
    }

    /// Bind a real (guest-minted) activity token to the pending launch the host
    /// recorded for it (W38).
    ///
    /// `startActivity` records the launch under the host's placeholder token;
    /// the app thread keys its record on the `IBinder` the launch transaction
    /// carried, which is created in the guest. The first `activityResumed` for
    /// an unknown token adopts it here: the placeholder is rewritten in the task
    /// stack and the class is returned so the ATMS record carries the truth. The
    /// launch leaves `pending_launches` because the guest materialised it (it is
    /// no longer work the runtime owes).
    pub fn adopt_launch_token(&mut self, real: u64) -> Option<(u64, String)> {
        if self.activities.iter().any(|a| a.token == real) {
            return None;
        }
        // MERGEFIX2: adoption is the **guest's** half of the seam - it binds the
        // token it minted to the launch it asked for. A host-origin launch (the
        // shade's sender) was never the guest's to materialise; it stays queued
        // for the peer's host-built delivery.
        let index = self
            .pending_launches
            .iter()
            .position(|launch| launch.origin == LaunchOrigin::Guest)?;
        let pending = self.pending_launches.remove(index);
        for activity in &mut self.activities {
            if activity.token == pending.token {
                activity.token = real;
            }
        }
        for task in &mut self.tasks {
            for token in &mut task.activities {
                if *token == pending.token {
                    *token = real;
                }
            }
        }
        Some((pending.token, pending.class))
    }

    /// MPROC7: the launch recorded for `package` that the host has not
    /// delivered yet (the first one in launch order).
    #[must_use]
    pub fn pending_launch_for(&self, package: &str) -> Option<&PendingLaunch> {
        self.pending_launches.iter().find(|l| l.package == package)
    }

    /// MERGEFIX2: the launch of `package` the **host** owes the process `tag`.
    ///
    /// Per-process, because the two processes of the ADR-0029 seam own
    /// different launches:
    ///
    /// * `tag 0` is the **primary** process. Its guest marshals its own
    ///   `LaunchActivityItem` and mints its own activity token (MPROC7's "the
    ///   primary process keeps the local path"), so the host owes it only what
    ///   the guest could not ask for itself: a [`LaunchOrigin::Host`] launch.
    ///   Delivering a guest's own launch again created the same activity
    ///   **twice** (two `MainActivity` instances, two `Application`-level
    ///   `React` roots: `gaten-backstack`'s first tap then never settled,
    ///   because the app re-rendered forever).
    /// * Any other tag is a spawned secondary: `ACOSLAUNCH_HOST_SCHEDULES` makes
    ///   its guest wait for the host's transaction, whatever the origin
    ///   (MPROC7, `LaunchMain`'s `waiting-for-host-transaction`).
    #[must_use]
    pub fn pending_launch_for_tag(&self, tag: u32, package: &str) -> Option<&PendingLaunch> {
        self.pending_launches.iter().find(|launch| {
            launch.package == package
                && (tag != crate::atms::PRIMARY_PROCESS_TAG || launch.origin == LaunchOrigin::Host)
        })
    }

    /// MPROC7: take one `onNewIntent` delivery owed to process `tag`
    /// (`singleTop` / `FLAG_ACTIVITY_SINGLE_TOP`). `new_intents` stays the
    /// evidence stream; this queue is what the peer drains into a
    /// `NewIntentItem`.
    pub fn take_pending_new_intent(&mut self, tag: u32) -> Option<(u64, IntentFacts)> {
        let index = self
            .pending_new_intents
            .iter()
            .position(|(token, _)| crate::atms::process_tag_of(*token) == tag)?;
        Some(self.pending_new_intents.remove(index))
    }

    /// MPROC7: bind the recorded launch of placeholder token `old` to `new`.
    ///
    /// The host mints the activity token for a launch it encodes itself (a
    /// running process, or a freshly spawned secondary whose first launch is no
    /// longer marshalled by its guest). The placeholder `start_activity`
    /// recorded is rewritten in the task stack, so the caller's
    /// `activityResumed(new)` finds the record in `AtmsState` and the
    /// `startActivityForResult` receiver (`result_to`, an activity token) still
    /// names the right activity.
    pub fn bind_launch_token(&mut self, old: u64, new: u64) {
        for activity in &mut self.activities {
            if activity.token == old {
                activity.token = new;
            }
        }
        for task in &mut self.tasks {
            for token in &mut task.activities {
                if *token == old {
                    *token = new;
                }
            }
        }
        for launch in &mut self.pending_launches {
            if launch.token == old {
                launch.token = new;
            }
            if launch.result_to == old {
                launch.result_to = new;
            }
        }
    }

    /// MPROC7: mark the launch of `token` as delivered (it leaves the queue; the
    /// activity record now carries the lifecycle).
    pub fn take_launch(&mut self, token: u64) -> Option<PendingLaunch> {
        let index = self
            .pending_launches
            .iter()
            .position(|l| l.token == token)?;
        Some(self.pending_launches.remove(index))
    }

    /// `IActivityManager.addPackageDependency(String packageName)`: record that
    /// the calling process depends on `packageName`, exactly like
    /// `ActivityManagerService.addPackageDependency` adds it to the caller's
    /// `ProcessRecord.pkgDeps`.
    ///
    /// Android 14's AIDL method takes **one** `String` (transaction 96): the
    /// image's `IActivityManager$Stub$Proxy.addPackageDependency` writes
    /// `writeString(packageName)` and nothing else, and `IActivityManager$Stub`
    /// reads one `readString()` before `writeNoException()` (checked with
    /// `dexdump` of `.cache/boot/android-34/dex/framework/classes.dex`; the AOSP
    /// source cache agrees — the `(String, int)` overload is a later platform).
    /// The reply is void.
    ///
    /// Returns `true` when the dependency was newly recorded. A repeat is not an
    /// error (`pkgDeps` is a set on a device too); it does not change the set.
    pub fn add_package_dependency(&mut self, package_name: &str) -> bool {
        self.package_dependencies.insert(package_name.to_string())
    }

    /// `finishActivity`: pop the activity from its task. Returns whether the
    /// token existed and was unfinished.
    pub fn finish_activity(&mut self, token: u64) -> bool {
        let Some(activity) = self.activities.iter_mut().find(|a| a.token == token) else {
            return false;
        };
        if activity.finished {
            return false;
        }
        activity.finished = true;
        let task_id = activity.task_id;
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == task_id) {
            task.activities.retain(|t| *t != token);
            if task.activities.is_empty() {
                self.tasks.retain(|t| t.id != task_id);
            }
        }
        true
    }

    /// `startService`: resolve and start the service. Returns the component.
    /// `IIntentSender.send`: replay the sender's stored intent(s).
    ///
    /// The record is looked up by the binder the caller used, so a cancelled
    /// record refuses (AOSP logs and returns; the refusal is recorded as
    /// evidence rather than thrown, exactly like `PendingIntentRecord.
    /// sendInner`). Delivery is the same path the app's own start would take:
    /// `start_activity` for a `getActivity` sender, [`AmsState::broadcast`] for
    /// `getBroadcast`, [`AmsState::start_service`] for `getService`.
    pub fn send_pending_intent(
        &mut self,
        binder: u64,
        code: i32,
    ) -> Result<crate::pending_intent::SendEvidence> {
        self.send_pending_intent_with_fill_in(binder, code, crate::pending_intent::FillIn::None)
    }

    /// [`Self::send_pending_intent`] with the caller's fill-in `Intent`, the
    /// way `PendingIntent.send(context, code, intent)` delivers it.
    ///
    /// The system side is `PendingIntentRecord.sendInner`
    /// (`services.dex`, disassembled in `results/handoff/PIFILLIN`):
    ///
    /// ```text
    /// finalIntent = new Intent(key.requestIntent)      // or new Intent()
    /// if ((key.flags & FLAG_IMMUTABLE) == 0) {         // 0x04000000 test
    ///     if (intent != null) finalIntent.fillIn(intent, key.flags);
    /// }
    /// ```
    ///
    /// so a **mutable** sender merges the fill-in through
    /// [`crate::fill_in::fill_in_value`] (with the record's flags as the
    /// `FILL_IN_*` mask, which — like AOSP — is 0 for every real
    /// `PendingIntent` flag, because those live above bit 8) and delivers the
    /// merged intent; an **immutable** sender ignores the fill-in entirely and
    /// the evidence records `fill_in_ignored`. Nothing is ever sent twice or
    /// half-merged.
    pub fn send_pending_intent_with_fill_in(
        &mut self,
        binder: u64,
        code: i32,
        fill_in: crate::pending_intent::FillIn<'_>,
    ) -> Result<crate::pending_intent::SendEvidence> {
        let Some(record) = self.intent_senders.record_cloned(binder) else {
            return Err(ServiceError::BadRequest {
                interface: "android.content.IIntentSender".into(),
                method: "send".into(),
                detail: format!("binder {binder} is not a live pending-intent sender"),
            });
        };
        let cancelled = record.cancelled;
        let immutable = record.flags & crate::pending_intent::FLAG_IMMUTABLE != 0;
        // `FLAG_UPDATE_CURRENT` stored extras are applied to every intent the
        // record replays (`PendingIntentRecord.sendInner` sets them on the
        // intent before the start); the fill-in merges *after* them, so a
        // fill-in key the stored intent also has still loses to the stored
        // value (AOSP's `newb.putAll(mExtras)`).
        let mut merged: Vec<(IntentFacts, i32)> = Vec::with_capacity(record.intents.len());
        for stored in &record.intents {
            let mut intent = stored.clone();
            for (key, value) in record
                .extras
                .as_ref()
                .map(crate::pending_intent::bundle_extras)
                .unwrap_or_default()
            {
                intent.extras.insert(key, value);
            }
            let mut changes = 0;
            if !immutable {
                if let Some(fill_in) = fill_in.intent() {
                    changes = intent.fill_in(fill_in, record.flags);
                }
            }
            merged.push((intent, changes));
        }
        let (intent, fill_in_changes) = merged
            .first()
            .cloned()
            .unwrap_or_else(|| (IntentFacts::default(), 0));
        let outcome = if cancelled {
            Err("the pending intent was cancelled".to_string())
        } else {
            match record.kind {
                // `INTENT_SENDER_ACTIVITY` and `INTENT_SENDER_ACTIVITY_RESULT`
                // both start the stored activity; the result half (its
                // `resultTo`/`resultWho`) is not modelled by `start_activity`,
                // which is what a tap/notification sender uses.
                crate::pending_intent::TYPE_ACTIVITY
                | crate::pending_intent::TYPE_ACTIVITY_RESULT => self
                    .start_activity_from_host(&intent, 0, 0, record.request_code)
                    .map_err(|e| e.to_string()),
                crate::pending_intent::TYPE_BROADCAST => self
                    .broadcast(&intent, 0, code, None)
                    .map_err(|e| e.to_string()),
                crate::pending_intent::TYPE_SERVICE
                | crate::pending_intent::TYPE_FOREGROUND_SERVICE => self
                    .start_service(&intent)
                    .map(|_| 0)
                    .map_err(|e| e.to_string()),
                other => Err(format!("unmodelled sender type {other}")),
            }
        };
        let evidence = crate::pending_intent::SendEvidence {
            code,
            outcome: outcome.clone(),
            intent,
            refused_cancelled: cancelled,
            fill_in_ignored: fill_in.is_some() && immutable,
            fill_in: fill_in.intent().cloned(),
            fill_in_changes,
        };
        self.intent_senders.record_send(binder, evidence.clone());
        Ok(evidence)
    }

    pub fn start_service(&mut self, intent: &IntentFacts) -> Result<(String, String)> {
        let component = self.manifests.resolve_service(intent)?;
        // MPROC4 step 3b: the service's code runs in its package's process.
        self.request_process(&component.package, &component.class, "startService");
        let key = format!("{}/{}", component.package, component.class);
        self.services
            .entry(key)
            .and_modify(|s| s.started = true)
            .or_insert(ServiceRecord {
                package: component.package.clone(),
                class: component.class.clone(),
                started: true,
                bind_count: 0,
            });
        Ok((component.package, component.class))
    }

    /// `stopService`: stop a started service. Returns whether anything stopped.
    pub fn stop_service(&mut self, intent: &IntentFacts) -> Result<bool> {
        let component = self.manifests.resolve_service(intent)?;
        let key = format!("{}/{}", component.package, component.class);
        let Some(service) = self.services.get_mut(&key) else {
            return Ok(false);
        };
        let stopped = service.started;
        service.started = false;
        if service.bind_count == 0 {
            self.services.remove(&key);
        }
        Ok(stopped)
    }

    /// `bindService`: resolve the service and queue `connected(connection, binder)`.
    /// Returns the synthetic service binder.
    pub fn bind_service(
        &mut self,
        connection: u64,
        intent: &IntentFacts,
        _flags: i64,
    ) -> Result<u64> {
        let component = self.manifests.resolve_service(intent)?;
        // MPROC4 step 3b: a bound service's code runs in its package's process.
        self.request_process(&component.package, &component.class, "bindService");
        let key = format!("{}/{}", component.package, component.class);
        self.services
            .entry(key)
            .and_modify(|s| s.bind_count += 1)
            .or_insert(ServiceRecord {
                package: component.package.clone(),
                class: component.class.clone(),
                started: false,
                bind_count: 1,
            });
        // The service binder is named by the runtime's real `Binder`; ACOS's
        // VM-free layer names it with a stable synthetic id derived from the
        // connection so a delivery can be matched. The runtime replaces it with
        // the real local `Binder` when the guest `Service.onBind` returns one.
        let binder = 0x5000_0000_0000_0000 | (connection & 0x0000_ffff_ffff_ffff);
        self.pending_binds.push(PendingBind {
            connection,
            package: component.package,
            class: component.class,
            binder,
        });
        Ok(binder)
    }

    /// Record that `package` needs a process (MPROC4 step 3b).
    ///
    /// One entry per `(package, component, reason)`: a resolution storm from one
    /// guest must not turn into a spawn storm. The launcher dedups again against
    /// its live processes (it is the authority on what is already running).
    pub fn request_process(&mut self, package: &str, class: &str, reason: &str) {
        if package.is_empty() {
            return;
        }
        let component = if class.is_empty() {
            package.to_string()
        } else {
            format!("{package}/{class}")
        };
        if self
            .spawn_requests
            .iter()
            .any(|r| r.component == component && r.reason == reason)
        {
            return;
        }
        if self.spawn_requests.len() >= MAX_SPAWN_REQUESTS {
            return;
        }
        self.spawn_requests.push(ProcessSpawnRequest {
            package: package.to_string(),
            component,
            reason: reason.to_string(),
        });
    }

    /// Drain the queued spawn requests (the launcher's side of step 3b).
    pub fn take_spawn_requests(&mut self) -> Vec<ProcessSpawnRequest> {
        std::mem::take(&mut self.spawn_requests)
    }

    /// `unbindService`: drop one connection. Returns whether one was released.
    pub fn unbind_service(&mut self, connection: u64) -> bool {
        let Some(pending) = self
            .pending_binds
            .iter()
            .position(|p| p.connection == connection)
        else {
            return false;
        };
        let pending = self.pending_binds.remove(pending);
        let key = format!("{}/{}", pending.package, pending.class);
        if let Some(service) = self.services.get_mut(&key) {
            service.bind_count = service.bind_count.saturating_sub(1);
            if service.bind_count == 0 && !service.started {
                self.services.remove(&key);
            }
        }
        true
    }

    /// `registerReceiver`: record a dynamic `IIntentReceiver`.
    ///
    /// `actions`/`categories` are the subset the runtime-side AMS bridge decodes;
    /// a caller that has the whole `IntentFilter` (the ART host decodes every
    /// fact through `IntentFilter.writeToParcel`) registers with
    /// [`Self::register_receiver_with_filter`] so data/type matching is the
    /// filter's own.
    pub fn register_receiver(
        &mut self,
        receiver: u64,
        package: &str,
        actions: Vec<String>,
        categories: Vec<String>,
        flags: i32,
    ) {
        self.register_receiver_with_filter(
            receiver,
            package,
            ReceiverFilter {
                actions,
                categories,
                ..ReceiverFilter::default()
            },
            flags,
        );
    }

    /// `registerReceiver` with the full `IntentFilter` facts.
    pub fn register_receiver_with_filter(
        &mut self,
        receiver: u64,
        package: &str,
        filter: ReceiverFilter,
        flags: i32,
    ) {
        // `RECEIVER_EXPORTED`/`RECEIVER_NOT_EXPORTED` (`0x2`/`0x4`) are the
        // API-34 requirement; the permission checks that apply to an exported
        // registration are enforced by the caller (AMS `registerReceiver`
        // reads the filter's `mRequiredPermission`), and this host has no
        // per-receiver permission grant to consult yet.
        let _ = flags;
        self.receivers.retain(|r| r.receiver != receiver);
        self.receivers.push(ReceiverRecord {
            receiver,
            package: package.to_string(),
            filter,
            flags,
        });
    }

    /// `unregisterReceiver`: drop a dynamic receiver.
    pub fn unregister_receiver(&mut self, receiver: u64) -> bool {
        let before = self.receivers.len();
        self.receivers.retain(|r| r.receiver != receiver);
        self.receivers.len() != before
    }

    /// `broadcastIntent`: queue deliveries to every matching dynamic and
    /// manifest receiver. Returns the number of receivers the intent reached.
    ///
    /// The rules are `BroadcastQueue`'s for an **unordered** broadcast:
    ///
    /// * a package-targeted intent (`Intent.setPackage`) reaches only that
    ///   package's receivers — `BroadcastQueue` skips every other package's
    ///   registered receivers and manifest declarations;
    /// * `FLAG_RECEIVER_REGISTERED_ONLY` reaches only dynamically registered
    ///   receivers (no manifest `<receiver>` is instantiated);
    /// * every matching receiver is reached, in the same process or not, and
    ///   `IntentFilter.match` decides what "matching" means
    ///   ([`ReceiverFilter::matches`]); priority is recorded, never used,
    ///   because unordered delivery has no order;
    /// * a manifest match is queued as a [`PendingManifestReceiver`]: the
    ///   receiver class has to be instantiated **in its own app process**
    ///   (`IApplicationThread.scheduleReceiver` → `ActivityThread
    ///   .handleReceiver`), which the host does when it converts the queue;
    /// * ordered and sticky broadcasts are **not** modelled: an ordered
    ///   broadcast (`resultTo != null`) needs sequential delivery with
    ///   `getResultCode`/`getResultData`/`abortBroadcast`, and a sticky
    ///   broadcast needs a sticky store (this host keeps none, which is why
    ///   `registerReceiver` answers a null intent). Both are structured
    ///   refusals rather than a silent, wrong delivery.
    pub fn broadcast(
        &mut self,
        intent: &IntentFacts,
        result_to: u64,
        result_code: i32,
        result_data: Option<String>,
    ) -> Result<i32> {
        if result_to != 0 {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: "android.app.IActivityManager".into(),
                code: 14,
                method: "sendOrderedBroadcast (ordered delivery needs sequential receivers,                          getResultCode/getResultData and abortBroadcast; this host delivers                          broadcast receivers in parallel)"
                    .into(),
            });
        }
        // A sticky broadcast is `broadcastIntent(..., sticky = true)`, which
        // arrives with `FLAG_RECEIVER_STICKY`-style semantics only through the
        // sticky store; nothing distinguishes it on this wire, so an app that
        // calls `sendStickyBroadcast` gets the honest "no sticky store" answer
        // from `registerReceiver` (a null Intent) rather than a fabricated one.
        let registered_only = intent.flags & FLAG_RECEIVER_REGISTERED_ONLY != 0;
        let target = intent.package.as_deref().filter(|p| !p.is_empty());
        let mut matched = 0;

        if !registered_only {
            for component in self.manifests.resolve_receivers(intent) {
                if let Some(target) = target {
                    if component.package != target {
                        continue;
                    }
                }
                self.pending_manifest_receivers.push(PendingManifestReceiver {
                    package: component.package.clone(),
                    class: component.class.clone(),
                    intent: intent.clone(),
                    sending_package: intent.package.clone().unwrap_or_default(),
                });
                matched += 1;
            }
        }

        let mut dynamic: Vec<u64> = Vec::new();
        for receiver in &self.receivers {
            // An empty `package` is a registration whose caller did not name
            // its package (the runtime-side `registerReceiver` mirror); a
            // package-targeted broadcast cannot exclude what it cannot name.
            if let Some(target) = target {
                if !receiver.package.is_empty() && receiver.package != target {
                    continue;
                }
            }
            if receiver.filter.matches(intent) {
                dynamic.push(receiver.receiver);
            }
        }
        for receiver in dynamic {
            self.pending_receivers.push(PendingReceiver {
                receiver,
                intent: intent.clone(),
                result_code,
                result_data: result_data.clone(),
                ordered: false,
            });
            matched += 1;
        }

        // A broadcast nobody listens to is not an error on Android.
        Ok(matched)
    }

    /// `getContentProvider`: the published provider holder for `authority`.
    #[must_use]
    pub fn get_content_provider(&self, authority: &str) -> Option<ProviderRecord> {
        if authority == "settings" {
            return Some(ProviderRecord {
                authority: "settings".into(),
                package: "com.android.providers.settings".into(),
                class: "SettingsProvider".into(),
                binder: 0,
            });
        }
        self.providers.get(authority).cloned()
    }

    /// `publishContentProviders`: record the app's provider binders.
    pub fn publish_content_providers(&mut self, entries: Vec<ProviderRecord>) {
        for entry in entries {
            self.providers.insert(entry.authority.clone(), entry);
        }
    }

    /// Drain the queued launches (runtime materialises the transactions).
    pub fn take_pending_launches(&mut self) -> Vec<PendingLaunch> {
        std::mem::take(&mut self.pending_launches)
    }

    /// Drain the queued `IServiceConnection` deliveries.
    pub fn take_pending_binds(&mut self) -> Vec<PendingBind> {
        std::mem::take(&mut self.pending_binds)
    }

    /// Drain the queued `IIntentReceiver` deliveries.
    pub fn take_pending_receivers(&mut self) -> Vec<PendingReceiver> {
        std::mem::take(&mut self.pending_receivers)
    }

    /// Drain the manifest-receiver matches the owning process must instantiate.
    pub fn take_pending_manifest_receivers(&mut self) -> Vec<PendingManifestReceiver> {
        std::mem::take(&mut self.pending_manifest_receivers)
    }

    fn task_for(&mut self, _component: &DeclaredComponent, behaviour: LaunchBehaviour) -> i32 {
        if behaviour == LaunchBehaviour::NewTask || self.tasks.is_empty() {
            let id = self.next_task_id;
            self.next_task_id += 1;
            self.tasks.push(Task {
                id,
                activities: Vec::new(),
            });
            return id;
        }
        self.tasks.last().map(|t| t.id).unwrap_or(1)
    }

    fn task_mut(&mut self, id: i32) -> &mut Task {
        if !self.tasks.iter().any(|t| t.id == id) {
            self.tasks.push(Task {
                id,
                activities: Vec::new(),
            });
        }
        self.tasks
            .iter_mut()
            .find(|t| t.id == id)
            .expect("task exists")
    }

    fn clear_above(&mut self, task_id: i32, token: u64) -> Vec<u64> {
        let Some(task) = self.tasks.iter_mut().find(|t| t.id == task_id) else {
            return Vec::new();
        };
        let Some(index) = task.activities.iter().position(|t| *t == token) else {
            return Vec::new();
        };
        task.activities.split_off(index + 1)
    }
}

// ---------------------------------------------------------------------------
// AIDL argument indices
// ---------------------------------------------------------------------------

/// The argument positions of a `startActivity*` transaction, pinned against the
/// Android 14 bytecode (`.cache/boot/android-34/dex/framework/classes.dex`,
/// `IActivityManager$Stub$Proxy`/`IActivityTaskManager$Stub$Proxy`).
///
/// `IActivityManager.startActivity` writes, in order: caller, callingPackage,
/// `Intent`, resolvedType, resultTo, resultWho, **requestCode**, **startFlags**,
/// profilerInfo, bOptions. `IActivityManager.startActivityWithFeature` inserts
/// `callingFeatureId` after `callingPackage`; `IActivityTaskManager.startActivity`
/// already has that field. The proxy disassembly in the evidence command
/// `dexdump .../framework/classes.dex` shows `writeInt` (requestCode)
/// immediately followed by `writeInt` (startFlags), which is what these
/// indices encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartActivityArgs {
    /// `Intent` argument (a `writeTypedObject`).
    pub intent: usize,
    /// `resultTo` (`IBinder`).
    pub result_to: usize,
    /// `resultWho` (`String`).
    pub result_who: usize,
    /// `requestCode` (`int`).
    pub request_code: usize,
    /// `startFlags` (`int`).
    pub flags: usize,
}

/// Argument positions for `startActivity`/`startActivityWithFeature`/
/// `startActivityAsUser` on either activity-manager interface.
///
/// Returns `None` for any other method.
#[must_use]
pub fn start_activity_args(interface: &str, method: &str) -> Option<StartActivityArgs> {
    let method_offset = match method {
        "startActivity" | "startActivityAsUser" => 0,
        // `startActivityWithFeature` inserts `String callingFeatureId` after
        // `String callingPackage` on `IActivityManager` only.
        "startActivityWithFeature" => 1,
        _ => return None,
    };
    // `IActivityTaskManager` carries `callingFeatureId` on every start.
    let interface_offset = usize::from(interface.ends_with("IActivityTaskManager"));
    let base = method_offset + interface_offset;
    Some(StartActivityArgs {
        intent: 2 + base,
        result_to: 4 + base,
        result_who: 5 + base,
        request_code: 6 + base,
        flags: 7 + base,
    })
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Dispatch an `IActivityManager`/`IActivityTaskManager` transaction to the
/// AMS state machine. Returns `Ok(None)` when the method is not an AMS method
/// (the caller then raises `UNSUPPORTED_ANDROID_API`).
pub fn dispatch(
    state: &mut AmsState,
    interface: &str,
    method: &MethodInfo,
    args: &[Value],
) -> Result<Option<CallResult>> {
    let value = |i: usize| args.get(i);
    let intent = |i: usize| -> Result<IntentFacts> {
        value(i)
            .and_then(Value::as_parcelable)
            .map(IntentFacts::from_parcelable)
            .or_else(|| value(i).and_then(Value::as_bundle).map(intent_from_bundle))
            .ok_or_else(|| ServiceError::BadRequest {
                interface: interface.to_string(),
                method: method.name.clone(),
                detail: format!("arg {i}: expected a reduced android.content.Intent"),
            })
    };

    let reply = match method.name.as_str() {
        "startActivity" | "startActivityWithFeature" | "startActivityAsUser" => {
            let positions = start_activity_args(interface, &method.name).ok_or_else(|| {
                ServiceError::BadRequest {
                    interface: interface.to_string(),
                    method: method.name.clone(),
                    detail: "no startActivity argument layout".into(),
                }
            })?;
            let intent = intent(positions.intent)?;
            // MODNOTIF2 / SYSUI1: `Activity.requestPermissions` starts an Intent
            // at the permission controller package (`buildRequestPermissionsIntent`,
            // `PackageManager.java:7049`). ADR-0029 amendment: the dialog is the
            // `com.android.permissioncontroller` system app. The host routes the
            // framework's own request there when that package is installed in
            // this session and the `permissions` module has dialogs on; otherwise
            // the structured `PERMISSION_UI_UNAVAILABLE` stands (never a silent
            // grant, never "activity not found", which looks like an app bug).
            let intent = match crate::permissions::permission_request(&intent) {
                Some(request) => {
                    if !state.permission_dialog_available(&intent) {
                        return Err(state.permission_ui_unavailable(&request));
                    }
                    state.record_permission_request(&request);
                    state.permission_dialog_intent(&request, &intent)
                }
                None => intent,
            };
            let flags = value(positions.flags).and_then(Value::as_int).unwrap_or(0);
            let result_to = value(positions.result_to)
                .and_then(Value::as_binder)
                .map(|b| b.0)
                .unwrap_or(0);
            let request_code = value(positions.request_code)
                .and_then(Value::as_int)
                .unwrap_or(0);
            let result_who = value(positions.result_who)
                .and_then(Value::as_str)
                .map(str::to_string);
            let code = match state.start_activity_with_who(
                &intent,
                flags,
                result_to,
                request_code,
                result_who,
            ) {
                Ok(code) => code,
                Err(ServiceError::IllegalState { .. }) => START_NOT_FOUND,
                Err(other) => return Err(other),
            };
            vec![Value::Int(code)]
        }
        "startService" => {
            let intent = intent(1)?;
            let (package, class) = state.start_service(&intent)?;
            vec![Value::TypedObject(component_name_parcelable(
                &package, &class,
            ))]
        }
        "stopService" => {
            let intent = intent(1)?;
            vec![Value::Int(i32::from(state.stop_service(&intent)?))]
        }
        "bindService" | "bindServiceInstance" => {
            let connection = value(4)
                .and_then(Value::as_binder)
                .map(|b| b.0)
                .unwrap_or(0);
            let intent = intent(2)?;
            let flags = value(5).and_then(Value::as_long).unwrap_or(0);
            let binder = state.bind_service(connection, &intent, flags)?;
            vec![Value::Int(i32::try_from(binder & 0x7fff_ffff).unwrap_or(0))]
        }
        "serviceDoneExecuting" => {
            // `ActivityThread` reports the end of a service lifecycle call
            // (`SERVICE_DONE_EXECUTING_*`) here; AOSP's AMS has nothing left to
            // do for a service it started, so the call is a no-op with a void
            // reply. A system-initiated accessibility bind runs it right after
            // `Service.onCreate`.
            Vec::new()
        }
        "unbindService" => {
            let connection = value(0)
                .and_then(Value::as_binder)
                .map(|b| b.0)
                .unwrap_or(0);
            vec![Value::Bool(state.unbind_service(connection))]
        }
        "registerReceiver" | "registerReceiverWithFeature" => {
            let receiver_index = if method.name == "registerReceiver" {
                2
            } else {
                4
            };
            let filter_index = receiver_index + 1;
            let receiver = value(receiver_index)
                .and_then(Value::as_binder)
                .map(|b| b.0)
                .unwrap_or(0);
            let actions = value(filter_index)
                .and_then(Value::as_parcelable)
                .and_then(|p| p.field(0))
                .map(string_list)
                .unwrap_or_default();
            let categories = value(filter_index)
                .and_then(Value::as_parcelable)
                .and_then(|p| p.field(1))
                .map(string_list)
                .unwrap_or_default();
            let flags = value(filter_index + 3).and_then(Value::as_int).unwrap_or(0);
            state.register_receiver(receiver, "", actions, categories, flags);
            vec![Value::TypedObject(Parcelable::Null)]
        }
        "unregisterReceiver" => {
            let receiver = value(0)
                .and_then(Value::as_binder)
                .map(|b| b.0)
                .unwrap_or(0);
            state.unregister_receiver(receiver);
            Vec::new()
        }
        "broadcastIntent" | "broadcastIntentWithFeature" => {
            let (intent_index, result_index, code_index, data_index) =
                if method.name == "broadcastIntent" {
                    (1, 3, 4, 5)
                } else {
                    (2, 4, 5, 6)
                };
            let intent = intent(intent_index)?;
            let result_to = value(result_index)
                .and_then(Value::as_binder)
                .map(|b| b.0)
                .unwrap_or(0);
            let result_code = value(code_index).and_then(Value::as_int).unwrap_or(0);
            let result_data = value(data_index)
                .and_then(Value::as_str)
                .map(str::to_string);
            let count = state.broadcast(&intent, result_to, result_code, result_data)?;
            vec![Value::Int(count)]
        }
        "getContentProvider" | "getContentProviderExternal" => {
            // `getContentProvider(IApplicationThread, String callingPackage,
            // String name, ...)` and `getContentProviderExternal(String name, ...)`
            // both carry the authority as arg 1.
            let authority = value(1).and_then(Value::as_str).unwrap_or_default();
            match state.get_content_provider(authority) {
                Some(provider) => vec![Value::TypedObject(provider_holder_parcelable(&provider))],
                None => vec![Value::TypedObject(Parcelable::Null)],
            }
        }
        "publishContentProviders" => {
            // The list elements are `ContentProviderHolder`s reduced by
            // `ams_java` to `ProviderRecord`-shaped parcelables.
            if let Some(Value::List(Some(items))) = args.get(1) {
                let mut records = Vec::new();
                for item in items {
                    if let Some(p) = item.as_parcelable() {
                        if let Some(record) = provider_from_holder(p) {
                            records.push(record);
                        }
                    }
                }
                state.publish_content_providers(records);
            }
            Vec::new()
        }
        // W53/WV3 (wall 5 of the WebView provider load):
        // `WebViewFactory.getWebViewContextAndSetProvider` registers the chosen
        // provider package with AMS before it reads that package's
        // `PackageInfo`, so a provider update kills the process that holds a
        // stale record. The faithful answer is the recorded dependency and a
        // void reply (the transport writes `writeNoException` only).
        "addPackageDependency" => {
            let package =
                value(0)
                    .and_then(Value::as_str)
                    .ok_or_else(|| ServiceError::BadRequest {
                        interface: interface.to_string(),
                        method: method.name.clone(),
                        detail: "arg 0: expected the package name (String)".into(),
                    })?;
            if package.is_empty() {
                // AOSP only records a name it can resolve; an empty dependency
                // is never a real package and would break the kill-on-update
                // contract the call exists for.
                return Err(ServiceError::BadRequest {
                    interface: interface.to_string(),
                    method: method.name.clone(),
                    detail: "arg 0: empty package name".into(),
                });
            }
            state.add_package_dependency(package);
            Vec::new()
        }
        // K2/R3: read-only queries. The guest's real memory state is the
        // host's `/proc/meminfo` (the guest runs in the host's memory cgroup);
        // an empty running-process list is the honest answer for a single
        // guest, and no other process belongs to this user in ACOS.
        "getMemoryInfo" => {
            let info = memory_info_parcelable();
            vec![Value::Cond {
                present: true,
                values: vec![Value::Parcelable(info)],
            }]
        }
        "getRunningAppProcesses" => vec![Value::List(Some(Vec::new()))],
        // MODNOTIF4: `PendingIntent` senders. The argument layout is the
        // decoded table's (`tables.rs:2068` for code 62, `:2091` for 63):
        // `(type, [featureId,] packageName, activity, who, requestCode,
        // Intent[] intents, String[] resolvedTypes, flags, Bundle options,
        // userId)`.
        "getIntentSender" | "getIntentSenderWithFeature" => {
            // HYG2/PROCUID §7: the `WithFeature` variant inserts the
            // `featureId` **after** the package
            // (`getIntentSenderWithFeature(int type, String packageName,
            // String featureId, IBinder token, ...)`, image AIDL code 63), so
            // the package is argument 1 in *both* forms and only the arguments
            // from `resultWho` on shift by one. Reading `1 + base` here took
            // the feature id as the package, which is null for every
            // `PendingIntent.getBroadcast/getActivity` (the framework passes a
            // null feature) — so the record's package was empty and
            // `PendingIntent.getCreatorPackage()` answered "".
            let base = usize::from(method.name == "getIntentSenderWithFeature");
            let kind = value(0).and_then(Value::as_int).unwrap_or(0);
            let package = value(1).and_then(Value::as_str).unwrap_or_default();
            let request_code = value(4 + base).and_then(Value::as_int).unwrap_or(0);
            let intents: Vec<IntentFacts> = match args.get(5 + base) {
                Some(Value::List(Some(items))) => items
                    .iter()
                    .filter_map(Value::as_parcelable)
                    .map(IntentFacts::from_parcelable)
                    .collect(),
                _ => Vec::new(),
            };
            let resolved_types: Vec<Option<String>> = match args.get(6 + base) {
                Some(Value::StringArray(Some(items))) => items.clone(),
                _ => vec![None; intents.len()],
            };
            let flags = value(7 + base).and_then(Value::as_int).unwrap_or(0);
            // `options` is a `Bundle` (the AIDL's last-but-one argument); its
            // one documented key for `PendingIntent` is the "pending intent
            // created for a shortcut/notification" marker, which the system
            // side stores and nothing in ACOS dereferences.
            let extras = match args.get(8 + base) {
                Some(Value::Bundle(Some(bundle))) if !bundle.is_empty() => Some(bundle.clone()),
                _ => None,
            };
            let binder = state.intent_senders.get_intent_sender(
                kind,
                package,
                request_code,
                flags,
                intents,
                resolved_types,
                extras,
            )?;
            vec![Value::Binder(binder.map(acos_parcel::BinderId))]
        }
        // `ActivityManagerService.isIntentSenderTargetedToPackage` (image dex
        // `services/classes.dex`, disassembled in
        // `results/handoff/PIFILLIN/pending-intent-dex.txt`): the framework asks
        // this before it hands a `PendingIntent` to a system service that will
        // broadcast through it (`LocationManager.requestLocationUpdates` does).
        // The image's rule is exactly "no stored intent carries both a package
        // and a component"; a binder that is not a sender record is false.
        "isIntentSenderTargetedToPackage" => {
            let binder = value(0).and_then(Value::as_binder).map(|b| b.0);
            let targeted = binder
                .and_then(|binder| state.intent_senders.record(binder))
                .map(|record| {
                    !record
                        .intents
                        .iter()
                        .any(|intent| intent.package.is_some() && intent.component.is_some())
                })
                .unwrap_or(false);
            vec![Value::Bool(targeted)]
        }
        "cancelIntentSender" => {
            let binder = value(0)
                .and_then(Value::as_binder)
                .map(|b| b.0)
                .unwrap_or(0);
            let cancelled = state.intent_senders.cancel(binder);
            vec![Value::Bool(cancelled)]
        }
        "getInfoForIntentSender" => {
            let binder = value(0)
                .and_then(Value::as_binder)
                .map(|b| b.0)
                .unwrap_or(0);
            // `ActivityManager.PendingIntentInfo` — the image's four fields
            // (`PendingIntentInfo.writeToParcel`: creator package, creator uid,
            // immutable, intent-sender type), i.e. what
            // `ActivityManagerService.getInfoForIntentSender` answers from the
            // record (`r.key.packageName, r.uid, r.key.flags & FLAG_IMMUTABLE,
            // r.key.type`). The creator uid is the **record's** — the process
            // that called `getIntentSender` — not the fixture uid: a pending
            // intent created by a second app reported the primary package's uid
            // (PROCUID README §7 item 2), and `PendingIntent.getCreatorUid()`
            // reads exactly this field. A binder that is not a record answers
            // the empty package with `immutable=false, type=0, uid=0`.
            let record = state.intent_senders.record(binder);
            vec![Value::TypedObject(Parcelable::ordered(
                "android.app.ActivityManager$PendingIntentInfo",
                vec![
                    Value::Str(Some(record.map(|r| r.package.clone()).unwrap_or_default())),
                    Value::Int(record.map(|r| r.uid).unwrap_or(0)),
                    Value::Bool(
                        record
                            .map(|r| r.flags & crate::pending_intent::FLAG_IMMUTABLE != 0)
                            .unwrap_or(false),
                    ),
                    Value::Int(record.map(|r| r.kind).unwrap_or(0)),
                ],
            ))]
        }
        _ => return Ok(None),
    };

    Ok(Some(CallResult {
        reply,
        deliveries: Vec::new(),
    }))
}

/// `android.app.ActivityManager$MemoryInfo` from the host's real memory state.
fn memory_info_parcelable() -> crate::value::Parcelable {
    let mut total_kb = 0u64;
    let mut avail_kb = 0u64;
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            match fields.next() {
                Some("MemTotal:") => {
                    total_kb = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                }
                Some("MemAvailable:") => {
                    avail_kb = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                }
                _ => {}
            }
        }
    }
    let total = total_kb.saturating_mul(1024);
    let avail = avail_kb.saturating_mul(1024);
    let threshold = total / 10;
    crate::value::Parcelable::ordered(
        "android.app.ActivityManager$MemoryInfo",
        vec![
            Value::Long(i64::try_from(avail).unwrap_or(0)),
            Value::Long(i64::try_from(total).unwrap_or(0)),
            Value::Long(i64::try_from(threshold).unwrap_or(0)),
            Value::Long(i64::try_from(total).unwrap_or(0)),
            Value::Int(i32::from(avail < threshold)),
            Value::Long(i64::try_from(total / 8).unwrap_or(0)),
            Value::Long(i64::try_from(total / 6).unwrap_or(0)),
            Value::Long(i64::try_from(total / 4).unwrap_or(0)),
            Value::Long(i64::try_from(total / 3).unwrap_or(0)),
        ],
    )
}

fn intent_from_bundle(bundle: &Bundle) -> IntentFacts {
    let Some(p) = bundle.get("intent").and_then(Value::as_parcelable) else {
        return IntentFacts::default();
    };
    IntentFacts::from_parcelable(p)
}

fn component_name_parcelable(package: &str, class: &str) -> Parcelable {
    Parcelable::ordered(
        "android.content.ComponentName",
        vec![
            Value::Str(Some(package.to_string())),
            Value::Str(Some(class.to_string())),
        ],
    )
}

fn provider_holder_parcelable(provider: &ProviderRecord) -> Parcelable {
    let app = crate::pms::AppInfo::hello_views_fallback(std::path::Path::new("provider.apk"));
    let info = crate::pms::ProviderInfo {
        name: provider.class.clone(),
        package_name: provider.package.clone(),
        authority: provider.authority.clone(),
        process_name: provider.package.clone(),
        read_permission: String::new(),
        write_permission: String::new(),
        exported: false,
        grant_uri_permissions: false,
        init_order: 0,
        meta_data: Vec::new(),
    }
    .to_parcelable(&app);
    // `ContentProviderHolder.writeToParcel` (API 34).
    Parcelable::ordered(
        "android.app.ContentProviderHolder",
        vec![
            Value::Parcelable(info),
            Value::Binder(if provider.binder != 0 {
                Some(acos_parcel::BinderId(provider.binder))
            } else {
                None
            }),
            Value::Binder(None),
            Value::Int(1), // noReleaseNeeded
            Value::Int(1), // mLocal
        ],
    )
}

fn provider_from_holder(holder: &Parcelable) -> Option<ProviderRecord> {
    let info = holder.field(0)?.as_parcelable()?;
    Some(ProviderRecord {
        class: info.field(0)?.as_str()?.to_string(),
        package: info.field(1)?.as_str()?.to_string(),
        authority: info.field(17)?.as_str()?.to_string(),
        binder: holder
            .field(1)
            .and_then(Value::as_binder)
            .map(|b| b.0)
            .unwrap_or(0),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_fixture() -> ManifestDb {
        let path = crate::workspace_path("fixtures/apps/service-provider/AndroidManifest.xml");
        // Build from the bundled manifest via a minimal APK is not possible for
        // a bare XML file; use a hand-built record matching the fixture instead.
        let _ = path;
        let mut db = ManifestDb::new();
        db.packages.push(PackageRecord {
            package: "dev.acos.fixtures.svcprovider".into(),
            uid: crate::pms::FIXTURE_UID,
            activities: vec![DeclaredComponent {
                package: "dev.acos.fixtures.svcprovider".into(),
                class: "dev.acos.fixtures.svcprovider.MainActivity".into(),
                exported: true,
                launch_mode: 0,
                filters: vec![Filter {
                    actions: vec!["android.intent.action.MAIN".into()],
                    categories: vec!["android.intent.category.LAUNCHER".into()],
                    ..Filter::default()
                }],
            }],
            services: vec![DeclaredComponent {
                package: "dev.acos.fixtures.svcprovider".into(),
                class: "dev.acos.fixtures.svcprovider.EchoService".into(),
                exported: false,
                launch_mode: 0,
                filters: vec![],
            }],
            receivers: vec![DeclaredComponent {
                package: "dev.acos.fixtures.svcprovider".into(),
                class: "dev.acos.fixtures.svcprovider.EchoReceiver".into(),
                exported: false,
                launch_mode: 0,
                filters: vec![Filter {
                    actions: vec!["dev.acos.fixtures.svcprovider.ECHO".into()],
                    ..Filter::default()
                }],
            }],
            providers: vec![DeclaredProvider {
                package: "dev.acos.fixtures.svcprovider".into(),
                class: "dev.acos.fixtures.svcprovider.EchoProvider".into(),
                authority: "dev.acos.fixtures.svcprovider".into(),
                exported: false,
            }],
        });
        db
    }

    #[test]
    fn explicit_intent_resolves_to_the_declared_component() {
        let db = db_with_fixture();
        let intent = IntentFacts::explicit(
            "dev.acos.fixtures.svcprovider",
            "dev.acos.fixtures.svcprovider.MainActivity",
        );
        let resolved = db.resolve_activity(&intent).expect("resolves");
        assert_eq!(resolved.class, "dev.acos.fixtures.svcprovider.MainActivity");
    }

    #[test]
    fn implicit_intent_resolves_by_action_and_category() {
        let db = db_with_fixture();
        let intent = IntentFacts {
            action: Some("android.intent.action.MAIN".into()),
            categories: vec!["android.intent.category.LAUNCHER".into()],
            ..IntentFacts::default()
        };
        assert!(db.resolve_activity(&intent).is_ok());
    }

    #[test]
    fn implicit_intent_with_no_match_is_a_structured_error() {
        let db = db_with_fixture();
        let intent = IntentFacts::action("com.example.NOPE");
        let err = db.resolve_activity(&intent).expect_err("must fail");
        assert!(matches!(
            err,
            ServiceError::IllegalState {
                component: "ams",
                ..
            }
        ));
        assert_eq!(err.code(), crate::codes::SERVICE_ILLEGAL_STATE);
    }

    #[test]
    fn start_activity_queues_a_launch_and_creates_a_task() {
        let mut state = AmsState::new();
        state.manifests = db_with_fixture();
        let intent = IntentFacts::explicit(
            "dev.acos.fixtures.svcprovider",
            "dev.acos.fixtures.svcprovider.MainActivity",
        );
        assert_eq!(
            state.start_activity(&intent, 0, 0, 0).expect("launch"),
            START_SUCCESS
        );
        let launches = state.take_pending_launches();
        assert_eq!(launches.len(), 1);
        assert_eq!(
            launches[0].class,
            "dev.acos.fixtures.svcprovider.MainActivity"
        );
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.tasks[0].activities.len(), 1);
    }

    #[test]
    fn single_task_reuses_the_existing_instance() {
        let mut state = AmsState::new();
        // A launcher plus a singleTask detail.
        let mut db = db_with_fixture();
        db.packages[0].activities.push(DeclaredComponent {
            package: "dev.acos.fixtures.svcprovider".into(),
            class: "dev.acos.fixtures.svcprovider.DetailActivity".into(),
            exported: false,
            launch_mode: 2,
            filters: vec![],
        });
        state.manifests = db;
        let detail = IntentFacts::explicit(
            "dev.acos.fixtures.svcprovider",
            "dev.acos.fixtures.svcprovider.DetailActivity",
        );
        state.start_activity(&detail, 0, 0, 0).unwrap();
        // The second singleTask launch reuses the top instance: no new launch.
        state.start_activity(&detail, 0, 0, 0).unwrap();
        assert_eq!(state.take_pending_launches().len(), 1);
        assert_eq!(state.new_intents.len(), 1);
    }

    #[test]
    fn service_start_stop_and_bind_round_trip() {
        let mut state = AmsState::new();
        state.manifests = db_with_fixture();
        let intent = IntentFacts::explicit(
            "dev.acos.fixtures.svcprovider",
            "dev.acos.fixtures.svcprovider.EchoService",
        );
        let (package, class) = state.start_service(&intent).expect("start");
        assert_eq!(class, "dev.acos.fixtures.svcprovider.EchoService");
        let key = format!("{package}/{class}");
        assert!(state.services[&key].started);

        let binder = state.bind_service(0xabc, &intent, 0).expect("bind");
        assert_eq!(state.pending_binds.len(), 1);
        assert_ne!(binder, 0);
        assert!(state.unbind_service(0xabc));
        assert_eq!(state.services[&key].bind_count, 0);

        assert!(state.stop_service(&intent).expect("stop"));
        assert!(!state.services.contains_key(&key));
        assert!(!state.stop_service(&intent).expect("second stop"));
    }

    #[test]
    fn manifest_and_dynamic_receivers_both_get_the_broadcast() {
        let mut state = AmsState::new();
        state.manifests = db_with_fixture();
        state.register_receiver(
            0x77,
            "dev.acos.fixtures.svcprovider",
            vec!["dev.acos.fixtures.svcprovider.ECHO".into()],
            vec![],
            0x2,
        );
        let intent = IntentFacts::action("dev.acos.fixtures.svcprovider.ECHO")
            .with_string_extra("payload", "hi");
        let count = state.broadcast(&intent, 0, 0, None).expect("broadcast");
        assert_eq!(count, 2, "one manifest + one dynamic receiver");
        let deliveries = state.take_pending_receivers();
        assert_eq!(deliveries.len(), 1, "the dynamic receiver's performReceive");
        assert!(matches!(
            deliveries[0].intent.extras.get("payload"),
            Some(Extra::String(s)) if s == "hi"
        ));
        // The manifest receiver is instantiated by its own process
        // (`scheduleReceiver`), so it is queued separately, never dropped.
        let manifest = state.take_pending_manifest_receivers();
        assert_eq!(manifest.len(), 1);
        assert_eq!(
            (manifest[0].package.as_str(), manifest[0].class.as_str()),
            (
                "dev.acos.fixtures.svcprovider",
                "dev.acos.fixtures.svcprovider.EchoReceiver"
            )
        );
    }

    #[test]
    fn an_ordered_broadcast_is_a_structured_unsupported() {
        let mut state = AmsState::new();
        state.manifests = db_with_fixture();
        let intent = IntentFacts::action("dev.acos.fixtures.svcprovider.ECHO");
        let err = state
            .broadcast(&intent, 0x99, 7, Some("done".into()))
            .expect_err("ordered delivery is not modelled");
        assert!(matches!(err, ServiceError::UnsupportedAndroidApi { .. }));
        assert!(
            state.take_pending_receivers().is_empty()
                && state.take_pending_manifest_receivers().is_empty(),
            "a refused broadcast delivers nothing at all"
        );
    }

    #[test]
    fn unknown_authority_returns_null_and_publish_registers_it() {
        let mut state = AmsState::new();
        let mut db = db_with_fixture();
        state.manifests = db.clone();
        assert!(state.get_content_provider("no.such.authority").is_none());
        let declared = db.packages[0].providers.remove(0);
        state.publish_content_providers(vec![ProviderRecord {
            authority: declared.authority.clone(),
            package: declared.package.clone(),
            class: declared.class.clone(),
            binder: 0x1234,
        }]);
        let provider = state
            .get_content_provider(&declared.authority)
            .expect("published");
        assert_eq!(provider.binder, 0x1234);
    }

    #[test]
    fn unsupported_launch_flags_are_a_structured_error() {
        let mut state = AmsState::new();
        state.manifests = db_with_fixture();
        let intent = IntentFacts::explicit(
            "dev.acos.fixtures.svcprovider",
            "dev.acos.fixtures.svcprovider.MainActivity",
        );
        let err = state
            .start_activity(&intent, 0x0000_0020, 0, 0)
            .expect_err("flags");
        assert!(matches!(err, ServiceError::UnsupportedAndroidApi { .. }));
    }

    fn dynamic_filter(actions: &[&str], categories: &[&str]) -> ReceiverFilter {
        ReceiverFilter {
            actions: actions.iter().map(|a| (*a).to_string()).collect(),
            categories: categories.iter().map(|c| (*c).to_string()).collect(),
            ..ReceiverFilter::default()
        }
    }

    /// `IntentFilter.match`: an intent with **no** action matches any filter
    /// (only an intent that names an action the filter does not declare is a
    /// mismatch), and categories are a pure subset rule — a filter declares
    /// what it accepts, never a default.
    #[test]
    fn the_action_and_category_rules_are_aosp_intent_filter_match() {
        let filter = dynamic_filter(&["a.ACTION"], &["c.ONE"]);
        let matches = |intent: &IntentFacts| filter.matches(intent);
        assert!(matches(&IntentFacts::action("a.ACTION")));
        assert!(!matches(&IntentFacts::action("a.OTHER")));
        // An intent with no action matches every filter.
        assert!(matches(&IntentFacts::default()));
        // Categories: every intent category must be declared by the filter.
        let mut intent = IntentFacts::action("a.ACTION");
        intent.categories = vec!["c.ONE".into()];
        assert!(matches(&intent));
        intent.categories.push("c.TWO".into());
        assert!(!matches(&intent), "an undeclared category never matches");
        // ... and a filter that declares a category does not require an intent
        // to carry it (there is no DEFAULT rule for receivers).
        assert!(matches(&IntentFacts::action("a.ACTION")));
        // A filter with no actions does not match an intent that names one.
        assert!(!dynamic_filter(&[], &[]).matches(&IntentFacts::action("a.ACTION")));
    }

    /// `IntentFilter.matchData`: a filter with no data constraint matches only
    /// an intent with neither a type nor a URI; a scheme filter requires the
    /// scheme; a type filter requires a matching MIME type (with `*` wildcards)
    /// and never matches an intent that carries none.
    #[test]
    fn the_data_and_type_rules_follow_intent_filter_match_data() {
        let mut filter = dynamic_filter(&["a.ACTION"], &[]);
        let mut intent = IntentFacts::action("a.ACTION");
        assert!(filter.matches(&intent), "no data constraint, no data");
        intent.data = Some("https://example.com/path".into());
        assert!(!filter.matches(&intent), "a data URI needs a scheme entry");
        intent.data = None;
        intent.mime_type = Some("text/plain".into());
        assert!(!filter.matches(&intent), "a type needs a type entry");

        filter.schemes = vec!["https".into()];
        intent.mime_type = None;
        intent.data = Some("https://example.com/path".into());
        assert!(filter.matches(&intent));
        intent.data = Some("http://example.com/path".into());
        assert!(!filter.matches(&intent), "another scheme does not match");
        // A scheme filter still matches when the intent also carries a type only
        // if the filter declares that type: `matchData` returns NO_MATCH_DATA
        // for a type with no type entries.
        intent.data = Some("https://example.com/path".into());
        intent.mime_type = Some("text/plain".into());
        assert!(!filter.matches(&intent));

        let mut typed = dynamic_filter(&["a.ACTION"], &[]);
        typed.schemes = vec!["https".into()];
        typed.data_types = vec!["text/*".into()];
        let mut with_type = IntentFacts::action("a.ACTION");
        with_type.data = Some("https://example.com/path".into());
        with_type.mime_type = Some("text/plain".into());
        assert!(typed.matches(&with_type));
        with_type.mime_type = Some("image/png".into());
        assert!(!typed.matches(&with_type));
        // `*/*` is the universal type.
        let mut any = typed.clone();
        any.data_types = vec!["*/*".into()];
        with_type.mime_type = Some("image/png".into());
        assert!(any.matches(&with_type));
    }

    /// Authorities, ports and paths are `IntentFilter.AuthorityEntry` /
    /// `PatternMatcher` matching, in the two pattern kinds Android itself uses
    /// for `<data android:path>` (`literal`/`prefix`).
    #[test]
    fn authorities_ports_and_paths_match_like_intent_filter() {
        let mut filter = dynamic_filter(&["a.ACTION"], &[]);
        filter.schemes = vec!["https".into()];
        filter.authorities = vec![
            DataAuthority {
                host: Some("api.example.com".into()),
                port: 8443,
                wild: false,
            },
            DataAuthority {
                host: Some("*.example.org".into()),
                port: -1,
                wild: true,
            },
        ];
        filter.paths = vec![DataPattern {
            pattern: Some("/v1/".into()),
            kind: 1, // PATTERN_PREFIX
        }];
        let mut intent = IntentFacts::action("a.ACTION");
        intent.data = Some("https://api.example.com:8443/v1/items".into());
        assert!(filter.matches(&intent));
        intent.data = Some("https://api.example.com:9443/v1/items".into());
        assert!(!filter.matches(&intent), "the declared port is required");
        intent.data = Some("https://api.example.com:8443/v2/items".into());
        assert!(!filter.matches(&intent), "the path prefix must match");
        intent.data = Some("https://cdn.example.org/anything".into());
        assert!(!filter.matches(&intent), "the path prefix still applies");
        filter.paths.clear();
        assert!(filter.matches(&intent), "`*.example.org` matches a sub-domain");
        intent.data = Some("https://example.org/anything".into());
        assert!(!filter.matches(&intent), "the bare domain is not a sub-domain");
        // `PATTERN_SIMPLE_GLOB`: `X*` repeats the previous character.
        filter.authorities.clear();
        filter.paths = vec![DataPattern {
            pattern: Some("/items/1*".into()),
            kind: 2,
        }];
        intent.data = Some("https://api.example.com/items/111".into());
        assert!(filter.matches(&intent));
        intent.data = Some("https://api.example.com/items/121".into());
        assert!(!filter.matches(&intent));
    }

    /// A package-targeted broadcast reaches only that package's receivers
    /// (`BroadcastQueue` skips every other package), and
    /// `FLAG_RECEIVER_REGISTERED_ONLY` reaches no manifest receiver.
    #[test]
    fn package_targeting_and_registered_only_follow_the_broadcast_queue() {
        let mut state = AmsState::new();
        state.manifests = db_with_fixture();
        state.register_receiver_with_filter(
            0x11,
            "com.example.one",
            dynamic_filter(&["a.ACTION"], &[]),
            0x2,
        );
        state.register_receiver_with_filter(
            0x22,
            "com.example.two",
            dynamic_filter(&["a.ACTION"], &[]),
            0x2,
        );
        state.register_receiver_with_filter(
            0x33,
            // A mirrored registration whose caller named no package (the
            // runtime-side registerReceiver) cannot be excluded by a target.
            "",
            dynamic_filter(&["a.ACTION"], &[]),
            0x0,
        );

        let mut targeted = IntentFacts::action("a.ACTION");
        targeted.package = Some("com.example.one".into());
        let count = state.broadcast(&targeted, 0, 0, None).unwrap();
        let delivered: Vec<u64> = state
            .take_pending_receivers()
            .iter()
            .map(|r| r.receiver)
            .collect();
        assert_eq!(count, 2, "the target package's receiver + the unnamed one");
        assert_eq!(delivered, vec![0x11, 0x33]);
        assert!(
            state.take_pending_manifest_receivers().is_empty(),
            "the fixture's manifest receiver is another package"
        );

        let mut registered_only = IntentFacts::action("dev.acos.fixtures.svcprovider.ECHO");
        registered_only.flags = FLAG_RECEIVER_REGISTERED_ONLY;
        state.broadcast(&registered_only, 0, 0, None).unwrap();
        assert!(
            state.take_pending_manifest_receivers().is_empty(),
            "REGISTERED_ONLY never instantiates a manifest receiver"
        );
        let plain = IntentFacts::action("dev.acos.fixtures.svcprovider.ECHO");
        state.broadcast(&plain, 0, 0, None).unwrap();
        assert_eq!(state.take_pending_manifest_receivers().len(), 1);
    }

    /// `FLAG_ACTIVITY_RESET_TASK_IF_NEEDED` (`0x00200000`) is what Gaten's
    /// `LauncherActivity` sets on the `startActivity` that opens `MainActivity`
    /// (`0x10200000` with `NEW_TASK`). AOSP resets the task to its root only
    /// when an existing task is reused for a new root; ACOS's launch is always
    /// that task's root, so the flag has nothing to reset and the launch must
    /// not be refused (measured: the strict set turned Gaten's own onboarding
    /// launch into `UnsupportedOperationException` in `LauncherActivity`).
    #[test]
    fn reset_task_if_needed_is_accepted_and_launches() {
        let mut state = AmsState::new();
        state.manifests = db_with_fixture();
        let intent = IntentFacts::explicit(
            "dev.acos.fixtures.svcprovider",
            "dev.acos.fixtures.svcprovider.MainActivity",
        );
        assert_eq!(
            state
                .start_activity(&intent, 0x1020_0000, 0, -1)
                .expect("RESET_TASK_IF_NEEDED launches"),
            START_SUCCESS
        );
        assert_eq!(state.take_pending_launches().len(), 1);
    }

    /// W74-A: the `startActivity*` argument index map, pinned against the
    /// Android 14 `IActivityManager`/`IActivityTaskManager` proxy bytecode
    /// (`.cache/boot/android-34/dex/framework/classes.dex`). The proxy writes
    /// `requestCode` immediately before `startFlags`; the table below is the
    /// reference, and the `acos_aidl` layout is checked alongside it.
    #[test]
    fn start_activity_argument_indices_match_android_14_aidl() {
        use acos_aidl::WireOp;
        // (interface, method, intent, resultTo, requestCode, startFlags)
        let expected = [
            ("android.app.IActivityManager", "startActivity", 2, 4, 6, 7),
            (
                "android.app.IActivityManager",
                "startActivityWithFeature",
                3,
                5,
                7,
                8,
            ),
            (
                "android.app.IActivityManager",
                "startActivityAsUser",
                2,
                4,
                6,
                7,
            ),
            (
                "android.app.IActivityTaskManager",
                "startActivity",
                3,
                5,
                7,
                8,
            ),
            (
                "android.app.IActivityTaskManager",
                "startActivityWithFeature",
                4,
                6,
                8,
                9,
            ),
            (
                "android.app.IActivityTaskManager",
                "startActivityAsUser",
                3,
                5,
                7,
                8,
            ),
        ];
        for (interface, method, intent, result_to, request_code, flags) in expected {
            let got = start_activity_args(interface, method).expect("layout");
            assert_eq!(
                (got.intent, got.result_to, got.request_code, got.flags),
                (intent, result_to, request_code, flags),
                "{interface}.{method}"
            );
            // `IActivityTaskManager.startActivityWithFeature` has no proxy in
            // the API-34 framework DEX (only `IActivityManager` has the
            // `*WithFeature` variant), so the table cannot cross-check that one
            // row; its indices still follow the same insertion rule.
            let Some(layout) = acos_aidl::lookup_by_name(interface, method) else {
                continue;
            };
            assert!(
                matches!(
                    &layout.args[got.intent],
                    WireOp::TypedObject { class, .. } if class == "android.content.Intent"
                ),
                "{interface}.{method}: arg {} is not the Intent",
                got.intent
            );
            assert!(
                matches!(&layout.args[got.result_to], WireOp::Binder { .. }),
                "{interface}.{method}: arg {} is not resultTo",
                got.result_to
            );
            assert_eq!(layout.args[got.request_code], WireOp::I32);
            assert_eq!(layout.args[got.flags], WireOp::I32);
            assert_eq!(
                got.flags,
                got.request_code + 1,
                "requestCode then startFlags"
            );
        }
        assert!(start_activity_args("android.app.IActivityManager", "startService").is_none());
    }

    /// Behavioural pin: `IActivityTaskManager.startActivity` (feature-id layout)
    /// must read requestCode from arg 7, not startFlags/resultWho. A wrong map
    /// would either reject `flags=0x20` (a non-zero startFlags) or record the
    /// wrong request code on the pending launch.
    #[test]
    fn dispatch_start_activity_reads_request_code_and_flags_at_the_atms_indices() {
        let mut state = AmsState::new();
        state.manifests = db_with_fixture();
        let method = acos_aidl::lookup_by_name("android.app.IActivityTaskManager", "startActivity")
            .expect("decoded");
        let intent = IntentFacts::explicit(
            "dev.acos.fixtures.svcprovider",
            "dev.acos.fixtures.svcprovider.MainActivity",
        );
        let args = vec![
            Value::Binder(None),                        // 0 caller
            Value::Str(None),                           // 1 callingPackage
            Value::Str(None),                           // 2 callingFeatureId
            Value::TypedObject(intent.to_parcelable()), // 3 Intent
            Value::Str(None),                           // 4 resolvedType
            Value::Binder(None),                        // 5 resultTo
            Value::Str(Some("who".into())),             // 6 resultWho
            Value::Int(42),                             // 7 requestCode
            Value::Int(0),                              // 8 startFlags
            Value::TypedObject(Parcelable::Null),       // 9 profilerInfo
            Value::TypedObject(Parcelable::Null),       // 10 bOptions
        ];
        let result = dispatch(
            &mut state,
            "android.app.IActivityTaskManager",
            method,
            &args,
        )
        .expect("dispatch")
        .expect("handled");
        assert_eq!(result.reply, vec![Value::Int(START_SUCCESS)]);
        let launches = state.take_pending_launches();
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0].request_code, 42);
        assert_eq!(
            launches[0].class,
            "dev.acos.fixtures.svcprovider.MainActivity"
        );
    }

    #[test]
    fn dispatch_resolves_start_service_through_the_decoded_table() {
        let mut state = AmsState::new();
        state.manifests = db_with_fixture();
        let method = acos_aidl::lookup_by_name("android.app.IActivityManager", "startService")
            .expect("decoded");
        let intent = IntentFacts::explicit(
            "dev.acos.fixtures.svcprovider",
            "dev.acos.fixtures.svcprovider.EchoService",
        );
        // startService(IApplicationThread, Intent, resolvedType, fg, callerPkg, callerFeature, userId)
        let args = vec![
            Value::Binder(None),
            Value::TypedObject(intent.to_parcelable()),
            Value::Str(None),
            Value::Bool(false),
            Value::Str(None),
            Value::Str(None),
            Value::Int(0),
        ];
        let result = dispatch(&mut state, "android.app.IActivityManager", method, &args)
            .expect("dispatch")
            .expect("handled");
        assert_eq!(result.reply.len(), 1);
        let component = result.reply[0].as_parcelable().expect("ComponentName");
        assert_eq!(
            component.field(1).and_then(Value::as_str),
            Some("dev.acos.fixtures.svcprovider.EchoService")
        );
    }

    /// Wall 5 of the WebView provider load (WV3):
    /// `IActivityManager.addPackageDependency(String)` is AMS surface, not a
    /// WebView hook, so it is pinned twice — against the image's own AIDL
    /// (transaction 96, one `String`, a void reply) and behaviourally through
    /// `dispatch` (the dependency is recorded, the reply carries no exception
    /// word, and the same package recorded twice is still one dependency).
    ///
    /// Fails before the dispatch arm existed: `dispatch` returned `Ok(None)`,
    /// which the service layer turns into
    /// `unsupported Android API: android.app.IActivityManager code 96` — the
    /// exact exception `WebViewFactory.getWebViewContextAndSetProvider` threw.
    #[test]
    fn add_package_dependency_is_recorded_and_replies_void() {
        use acos_aidl::WireOp;
        let method =
            acos_aidl::lookup_by_name("android.app.IActivityManager", "addPackageDependency")
                .expect("the table decodes addPackageDependency from the image");
        assert_eq!(
            method.code, 96,
            "IActivityManager.addPackageDependency code"
        );
        assert!(
            !method.oneway,
            "the framework's proxy calls readException() on the reply"
        );
        assert_eq!(
            method.args,
            vec![WireOp::Str16],
            "Android 14 writes one String (the (String, int) overload is newer)"
        );
        assert!(
            method.reply.is_empty(),
            "the reply is writeNoException() and nothing else"
        );

        let mut state = AmsState::new();
        let args = vec![Value::Str(Some("com.android.webview".into()))];
        let result = dispatch(&mut state, "android.app.IActivityManager", &method, &args)
            .expect("dispatch")
            .expect("addPackageDependency is handled, not UNSUPPORTED");
        assert!(
            result.reply.is_empty(),
            "a void method replies with the exception word only, got {:?}",
            result.reply
        );
        assert!(result.deliveries.is_empty());
        assert!(state.package_dependencies.contains("com.android.webview"));

        // Recording twice is the same dependency, not a second entry.
        let again = dispatch(&mut state, "android.app.IActivityManager", &method, &args)
            .expect("dispatch")
            .expect("handled");
        assert!(again.reply.is_empty());
        assert_eq!(state.package_dependencies.len(), 1);

        // An empty name is refused, not recorded: it could never be a package.
        let empty = dispatch(
            &mut state,
            "android.app.IActivityManager",
            &method,
            &[Value::Str(Some(String::new()))],
        );
        assert!(matches!(empty, Err(ServiceError::BadRequest { .. })));
        assert_eq!(state.package_dependencies.len(), 1);
    }
}
