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

use std::collections::BTreeMap;

use acos_aidl::MethodInfo;

use crate::error::{ServiceError, Result};
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
/// `Intent.mPackage`.
pub const INTENT_FIELD_PACKAGE: usize = crate::parcelable::INTENT_PACKAGE;
/// `Intent.mComponent` (`android.content.ComponentName`, 2 String16 fields).
pub const INTENT_FIELD_COMPONENT: usize = crate::parcelable::INTENT_COMPONENT;
/// `Intent.mCategories` (a String list).
pub const INTENT_FIELD_CATEGORIES: usize = crate::parcelable::INTENT_CATEGORIES;
/// `Intent.mExtras` (`Bundle`).
pub const INTENT_FIELD_EXTRAS: usize = crate::parcelable::INTENT_EXTRAS;

/// The typed intent facts the AMS state machine resolves against.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IntentFacts {
    /// `Intent.getAction()`.
    pub action: Option<String>,
    /// `Intent.getDataString()`.
    pub data: Option<String>,
    /// `Intent.getPackage()`.
    pub package: Option<String>,
    /// Explicit component `(package, class)`.
    pub component: Option<(String, String)>,
    /// `Intent.getCategories()`.
    pub categories: Vec<String>,
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
    /// `putExtra(boolean)`.
    Bool(bool),
    /// A value of a type ACOS records but does not interpret.
    Present,
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
        self.extras.insert(key.to_string(), Extra::String(value.into()));
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
        Self {
            action: f.get(INTENT_FIELD_ACTION).and_then(Value::as_str).map(str::to_string),
            data: f.get(INTENT_FIELD_DATA).and_then(Value::as_str).map(str::to_string),
            package: f.get(INTENT_FIELD_PACKAGE).and_then(Value::as_str).map(str::to_string),
            component,
            categories,
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
        Parcelable::ordered(
            crate::parcelable::INTENT_CLASS,
            vec![
                Value::Str(self.action.clone()),
                Value::Str(self.data.clone()),
                Value::Str(None),           // type
                Value::Str(None),           // identifier
                Value::Int(0),              // flags
                Value::Str(self.package.clone()),
                Value::Parcelable(component.unwrap_or(Parcelable::Null)),
                Value::Parcelable(Parcelable::Null), // sourceBounds
                Value::StringList(Some(
                    self.categories.iter().cloned().map(Some).collect(),
                )),
                Value::Parcelable(Parcelable::Null), // selector
                Value::Parcelable(Parcelable::Null), // clipData
                Value::Int(-2),                      // contentUserHint
                Value::Bundle(Some(encode_extras(&self.extras))),
                Value::Parcelable(Parcelable::Null), // originalIntent
            ],
        )
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
            Value::Int(i) => Extra::Int(*i),
            Value::Bool(b) => Extra::Bool(*b),
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
            Extra::Bool(b) => bundle.put(key, Value::Bool(*b)),
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
        if !categories.iter().all(|c| self.categories.iter().any(|f| f == c)) {
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

    /// All manifest receivers matching `intent`.
    #[must_use]
    pub fn resolve_receivers(&self, intent: &IntentFacts) -> Vec<DeclaredComponent> {
        let mut out = Vec::new();
        for package in &self.packages {
            for receiver in &package.receivers {
                if receiver
                    .filters
                    .iter()
                    .any(|f| f.matches_receiver(intent))
                {
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
                detail: format!(
                    "no {} matches action {:?}",
                    kind.as_str(),
                    intent.action
                ),
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

/// One registered `IIntentReceiver`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverRecord {
    /// `IIntentReceiver` binder.
    pub receiver: u64,
    /// Owning package.
    pub package: String,
    /// Filter actions.
    pub actions: Vec<String>,
    /// Filter categories.
    pub categories: Vec<String>,
    /// `registerReceiver` flags.
    pub flags: i32,
}

impl ReceiverRecord {
    fn matches(&self, intent: &IntentFacts) -> bool {
        let action_ok = match &intent.action {
            Some(action) => self.actions.iter().any(|a| a == action),
            None => false,
        };
        if !action_ok {
            return false;
        }
        let categories = if intent.categories.is_empty() {
            vec!["android.intent.category.DEFAULT".to_string()]
        } else {
            intent.categories.clone()
        };
        categories.iter().all(|c| self.categories.iter().any(|f| f == c))
            || self.categories.is_empty()
    }
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

/// A guest `startActivity` the runtime must turn into a real
/// `ClientTransaction`.
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
    /// Next task id.
    next_task_id: i32,
    /// Next activity token.
    next_token: u64,
    /// Last `onNewIntent` records (evidence).
    pub new_intents: Vec<(u64, IntentFacts)>,
}

impl AmsState {
    /// New empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_task_id: 1,
            next_token: 0x1000_0000,
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
        const NEW_TASK: i32 = 0x1000_0000;
        const CLEAR_TOP: i32 = 0x0400_0000;
        const HONOURED: i32 = NEW_TASK | CLEAR_TOP;
        if flags & !HONOURED != 0 {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: "android.app.IActivityManager".into(),
                code: 14,
                method: format!("startActivity flags {flags:#x}"),
            });
        }
        let component = self.manifests.resolve_activity(intent)?;
        let behaviour = if flags & NEW_TASK != 0 || component.launch_mode == 3 {
            LaunchBehaviour::NewTask
        } else {
            match component.launch_mode {
                1 => LaunchBehaviour::ReuseTop,
                2 => LaunchBehaviour::FinishAboveAndReuse,
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
        self.pending_launches.push(PendingLaunch {
            token,
            package: component.package.clone(),
            class: component.class.clone(),
            intent: intent.clone(),
            task_id,
            result_to,
            request_code,
        });
        Ok(START_SUCCESS)
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
        if self.pending_launches.is_empty() {
            return None;
        }
        let pending = self.pending_launches.remove(0);
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
    pub fn start_service(&mut self, intent: &IntentFacts) -> Result<(String, String)> {
        let component = self.manifests.resolve_service(intent)?;
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
    pub fn register_receiver(
        &mut self,
        receiver: u64,
        package: &str,
        actions: Vec<String>,
        categories: Vec<String>,
        flags: i32,
    ) {
        if flags & 0x2 != 0 {
            // `RECEIVER_EXPORTED`/`RECEIVER_NOT_EXPORTED` are the API-34
            // requirement; both are accepted (the fixture registers a
            // same-package receiver). Unknown flag bits are not.
        }
        self.receivers.retain(|r| r.receiver != receiver);
        self.receivers.push(ReceiverRecord {
            receiver,
            package: package.to_string(),
            actions,
            categories,
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
    /// `ordered` mirrors `Intent.FLAG_RECEIVER_FOREGROUND`-style sequencing:
    /// when `result_to` is non-zero the broadcast is ordered and the result
    /// code/data start at `result_code`/`result_data` and are delivered to the
    /// caller's `IIntentReceiver`. An `ordered` broadcast with no `resultTo`
    /// receiver is a structured error.
    pub fn broadcast(
        &mut self,
        intent: &IntentFacts,
        result_to: u64,
        result_code: i32,
        result_data: Option<String>,
    ) -> Result<i32> {
        let ordered = result_to != 0;
        let mut matched = 0;

        // Manifest receivers first (in declared order), then dynamic ones.
        let manifest = self.manifests.resolve_receivers(intent);
        for component in &manifest {
            self.pending_receivers.push(PendingReceiver {
                receiver: synthesize_receiver_binder(&component.package, &component.class),
                intent: intent.clone(),
                result_code,
                result_data: result_data.clone(),
                ordered: false,
            });
            matched += 1;
        }

        for receiver in &self.receivers {
            if receiver.matches(intent) {
                self.pending_receivers.push(PendingReceiver {
                    receiver: receiver.receiver,
                    intent: intent.clone(),
                    result_code,
                    result_data: result_data.clone(),
                    ordered,
                });
                matched += 1;
            }
        }

        if ordered {
            // A real ordered broadcast always has a `resultTo`; if no dynamic
            // receiver matched there is still the result callback to answer.
            self.pending_receivers.push(PendingReceiver {
                receiver: result_to,
                intent: intent.clone(),
                result_code,
                result_data,
                ordered: true,
            });
            matched += 1;
        } else if matched == 0 {
            // A broadcast nobody listens to is not an error on Android.
        }
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

fn synthesize_receiver_binder(package: &str, class: &str) -> u64 {
    // A stable, collision-free id for a manifest receiver's
    // `IIntentReceiver` stub. The runtime maps it back to the real
    // `LoadedApk.ReceiverDispatcher` binder.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in package.bytes().chain(b"/".iter().copied()).chain(class.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    0x6000_0000_0000_0000 | (hash & 0x0fff_ffff_ffff_ffff)
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
            let flags = value(positions.flags).and_then(Value::as_int).unwrap_or(0);
            let result_to = value(positions.result_to)
                .and_then(Value::as_binder)
                .map(|b| b.0)
                .unwrap_or(0);
            let request_code = value(positions.request_code)
                .and_then(Value::as_int)
                .unwrap_or(0);
            let code = match state.start_activity(&intent, flags, result_to, request_code) {
                Ok(code) => code,
                Err(ServiceError::IllegalState { .. }) => START_NOT_FOUND,
                Err(other) => return Err(other),
            };
            vec![Value::Int(code)]
        }
        "startService" => {
            let intent = intent(1)?;
            let (package, class) = state.start_service(&intent)?;
            vec![Value::TypedObject(component_name_parcelable(&package, &class))]
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
            let receiver_index = if method.name == "registerReceiver" { 2 } else { 4 };
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
            let result_data = value(data_index).and_then(Value::as_str).map(str::to_string);
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
        assert_eq!(
            resolved.class,
            "dev.acos.fixtures.svcprovider.MainActivity"
        );
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
        assert!(matches!(err, ServiceError::IllegalState { component: "ams", .. }));
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
        assert_eq!(launches[0].class, "dev.acos.fixtures.svcprovider.MainActivity");
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
        assert_eq!(deliveries.len(), 2);
        assert!(deliveries
            .iter()
            .any(|d| matches!(d.intent.extras.get("payload"), Some(Extra::String(s)) if s == "hi")));
    }

    #[test]
    fn ordered_broadcast_answers_the_result_receiver() {
        let mut state = AmsState::new();
        state.manifests = db_with_fixture();
        let intent = IntentFacts::action("dev.acos.fixtures.svcprovider.ECHO");
        let count = state.broadcast(&intent, 0x99, 7, Some("done".into())).unwrap();
        assert_eq!(count, 2, "manifest receiver + ordered result");
        let last = state.take_pending_receivers().pop().unwrap();
        assert!(last.ordered);
        assert_eq!(last.receiver, 0x99);
        assert_eq!(last.result_code, 7);
        assert_eq!(last.result_data.as_deref(), Some("done"));
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
        let provider = state.get_content_provider(&declared.authority).expect("published");
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
        let err = state.start_activity(&intent, 0x0000_0020, 0, 0).expect_err("flags");
        assert!(matches!(err, ServiceError::UnsupportedAndroidApi { .. }));
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
            ("android.app.IActivityManager", "startActivityAsUser", 2, 4, 6, 7),
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
            assert_eq!(got.flags, got.request_code + 1, "requestCode then startFlags");
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
        let method =
            acos_aidl::lookup_by_name("android.app.IActivityTaskManager", "startActivity")
                .expect("decoded");
        let intent = IntentFacts::explicit(
            "dev.acos.fixtures.svcprovider",
            "dev.acos.fixtures.svcprovider.MainActivity",
        );
        let args = vec![
            Value::Binder(None),                  // 0 caller
            Value::Str(None),                     // 1 callingPackage
            Value::Str(None),                     // 2 callingFeatureId
            Value::TypedObject(intent.to_parcelable()), // 3 Intent
            Value::Str(None),                     // 4 resolvedType
            Value::Binder(None),                  // 5 resultTo
            Value::Str(Some("who".into())),       // 6 resultWho
            Value::Int(42),                       // 7 requestCode
            Value::Int(0),                        // 8 startFlags
            Value::TypedObject(Parcelable::Null), // 9 profilerInfo
            Value::TypedObject(Parcelable::Null), // 10 bOptions
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
        assert_eq!(launches[0].class, "dev.acos.fixtures.svcprovider.MainActivity");
    }

    #[test]
    fn dispatch_resolves_start_service_through_the_decoded_table() {
        let mut state = AmsState::new();
        state.manifests = db_with_fixture();
        let method = acos_aidl::lookup_by_name(
            "android.app.IActivityManager",
            "startService",
        )
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
}
