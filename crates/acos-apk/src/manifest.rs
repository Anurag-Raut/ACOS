//! The typed Android manifest model, built on the AXML DOM.

use crate::arsc::ResourceTable;
use crate::axml::{TypedValue, XmlAttribute, XmlDocument, XmlNode};
use crate::error::Result;

// `ApplicationInfo` flag bits, from the API-34 image's `ApplicationInfo`
// (`.cache/aosp/base/core/java/android/content/pm/ApplicationInfo.java`).
pub const FLAG_DEBUGGABLE: i32 = 1 << 1;
pub const FLAG_HAS_CODE: i32 = 1 << 2;
pub const FLAG_ALLOW_TASK_REPARENTING: i32 = 1 << 5;
pub const FLAG_ALLOW_CLEAR_USER_DATA: i32 = 1 << 6;
pub const FLAG_TEST_ONLY: i32 = 1 << 8;
pub const FLAG_SUPPORTS_SMALL_SCREENS: i32 = 1 << 9;
pub const FLAG_SUPPORTS_NORMAL_SCREENS: i32 = 1 << 10;
pub const FLAG_SUPPORTS_LARGE_SCREENS: i32 = 1 << 11;
pub const FLAG_RESIZEABLE_FOR_SCREENS: i32 = 1 << 12;
pub const FLAG_SUPPORTS_SCREEN_DENSITIES: i32 = 1 << 13;
pub const FLAG_VM_SAFE_MODE: i32 = 1 << 14;
pub const FLAG_ALLOW_BACKUP: i32 = 1 << 15;
pub const FLAG_SUPPORTS_XLARGE_SCREENS: i32 = 1 << 19;
pub const FLAG_LARGE_HEAP: i32 = 1 << 20;
pub const FLAG_MULTIARCH: i32 = 1 << 21;
pub const FLAG_SUPPORTS_RTL: i32 = 1 << 22;
/// Set by the package manager on an installed package, not by `PackageParser`.
pub const FLAG_INSTALLED: i32 = 1 << 23;
pub const FLAG_IS_GAME: i32 = 1 << 25;
pub const FLAG_USES_CLEARTEXT_TRAFFIC: i32 = 1 << 27;
pub const FLAG_EXTRACT_NATIVE_LIBS: i32 = 1 << 28;
pub const FLAG_HARDWARE_ACCELERATED: i32 = 1 << 29;

/// `<uses-permission>`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct UsesPermission {
    pub name: String,
    pub max_sdk: Option<u32>,
}

/// `<uses-feature>`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct UsesFeature {
    pub name: String,
    pub required: Option<bool>,
    pub version: Option<u32>,
}

/// One `<permission>` the package **declares** (SYSUI3B).
///
/// `PackageParser.parsePermission` (`base/core/java/android/content/pm/PackageParser.java`,
/// tag android-14.0.0_r75) turns the element into a `PermissionInfo`; the
/// fields here are the ones an `IPermissionManager.getPermissionInfo` answer
/// needs. `label`/`description` keep a literal string attribute as written; a
/// `@string/...` reference stays unset (`None`), because resolving it would
/// need the declaring package's resource table — the store reports the gap
/// rather than inventing a label.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DeclaredPermission {
    /// `android:name` (already normalised by `PackageParser`: a bare class name
    /// gets the package prefix, exactly as the platform does).
    pub name: String,
    /// A literal `android:label` string, when the manifest gives one.
    pub label: Option<String>,
    /// A literal `android:description` string, when the manifest gives one.
    pub description: Option<String>,
    /// `android:permissionGroup` (fully qualified, as written).
    pub group: Option<String>,
    /// `android:protectionLevel` as written (`"dangerous"`, `"signature|privileged"`).
    pub protection_level: Option<String>,
    /// `android:backgroundPermission`.
    pub background_permission: Option<String>,
}

/// A typed `<meta-data>` value, stored exactly as `PackageParser.parseMetaData`
/// (`.cache/aosp/base/core/java/android/content/pm/PackageParser.java`, tag
/// android-14.0.0_r75) writes it into the owning `PackageItemInfo`'s `Bundle`:
///
/// ```text
/// v = sa.peekValue(android:resource);
/// if (v != null && v.resourceId != 0)         data.putInt(name, v.resourceId);
/// else if (v.type == TYPE_STRING)             data.putString(name, v.coerceToString());
/// else if (v.type == TYPE_INT_BOOLEAN)        data.putBoolean(name, v.data != 0);
/// else if (TYPE_FIRST_INT <= v.type <= TYPE_LAST_INT) data.putInt(name, v.data);
/// else if (v.type == TYPE_FLOAT)              data.putFloat(name, v.getFloat());
/// else                                        warn (RIGID_PARSER off: not stored)
/// ```
///
/// `TYPE_FIRST_INT..=TYPE_LAST_INT` is `0x10..=0x1f`, i.e. int dec, int hex and
/// the four colour types; all of them are `putInt`. A reference inside
/// `android:value` is **not** one of those types, so `parseMetaData` logs it and
/// stores nothing — only `android:resource` becomes a reference id.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetaDataValue {
    /// `android:value="text"` -> `Bundle.putString`.
    String(String),
    /// `android:value="3"` (int dec/hex, colour) -> `Bundle.putInt`.
    Int(i32),
    /// `android:value="true"` -> `Bundle.putBoolean`.
    Boolean(bool),
    /// `android:value="1.5"` -> `Bundle.putFloat`.
    Float(f32),
    /// `android:resource="@xml/x"` -> `Bundle.putInt(name, resourceId)`.
    Reference(u32),
}

/// One `<meta-data android:name="…" android:value|android:resource="…"/>`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MetaDataEntry {
    /// `android:name` (the `Bundle` key).
    pub name: String,
    /// The typed value `PackageParser` stores under that key.
    pub value: MetaDataValue,
}

/// One `<supports-screens>` attribute value for the accepted AXML forms.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SupportsScreensValue {
    /// aapt2 writes an explicit `true` as `TYPE_INT_BOOLEAN` data `-1`; AOSP's
    /// `v < 0` branch makes this bypass the targetSdk gates.
    True,
    /// aapt2 writes an explicit `false` as data `0`.
    False,
    /// An `@bool/...` reference. Stock `Resources.obtainAttributes` resolves
    /// references through `AssetManager` before `getInteger` sees the value,
    /// but ACOS has no `resources.arsc` handle in `Manifest::parse` /
    /// `application_flags` (plumbing it needs `Apk::manifest` + PMS changes),
    /// so this is recorded but the flag is conservatively left clear.
    Reference(u32),
}

/// `<supports-screens>` attributes, as `PackageParser.parseBaseApkCommon` reads
/// them (`.cache/aosp/base/core/java/android/content/pm/PackageParser.java`,
/// tag android-14.0.0_r75: defaults `:1993-1998`, parsing `:2244-2274`, flag
/// derivation `:2476-2503`).
///
/// AOSP starts every value at the "unknown" sentinel `1`, which is why `None`
/// (absent) is not `false`: the stock condition is
/// `v < 0 || (v > 0 && targetSdk >= gate)`, with gates DONUT (4) for
/// small/large/resizeable/anyDensity and GINGERBREAD (9) for xlarge;
/// `normalScreens` has no gate. Explicit `true` (`-1`) bypasses a gate and
/// explicit `false` (`0`) clears the bit. Integer-typed attributes are
/// malformed for these boolean declarations and are not modelled.
///
/// The parser's effective screen SDK is `targetSdkVersion`, which AOSP defaults
/// to `minSdkVersion` inside `<uses-sdk>` and to `0` when `<uses-sdk>` is
/// absent. [`Manifest::application_flags`] therefore uses
/// `target_sdk.or(min_sdk).unwrap_or(0)`, not the API-34 fallback used by
/// unrelated application attributes (`usesCleartextTraffic` etc.).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
pub struct SupportsScreens {
    pub small_screens: Option<SupportsScreensValue>,
    pub normal_screens: Option<SupportsScreensValue>,
    pub large_screens: Option<SupportsScreensValue>,
    pub xlarge_screens: Option<SupportsScreensValue>,
    pub resizeable: Option<SupportsScreensValue>,
    pub any_density: Option<SupportsScreensValue>,
}

/// The `<application>` element's attributes.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
pub struct ApplicationInfo {
    pub name: Option<String>,
    pub label: Option<String>,
    pub label_ref: Option<u32>,
    pub debuggable: Option<bool>,
    pub allow_backup: Option<bool>,
    pub extract_native_libs: Option<bool>,
    /// `android:hardwareAccelerated` (absent = application default, see PMS).
    pub hardware_accelerated: Option<bool>,
    /// W31: the rest of `ParsingPackageUtils.parseBaseApplication`'s
    /// `ApplicationInfo.flags` inputs. `None` means "attribute absent"; the
    /// stock default is applied in [`Manifest::application_flags`], never here.
    pub has_code: Option<bool>,
    pub uses_cleartext_traffic: Option<bool>,
    pub large_heap: Option<bool>,
    pub supports_rtl: Option<bool>,
    pub is_game: Option<bool>,
    pub vm_safe_mode: Option<bool>,
    pub allow_clear_user_data: Option<bool>,
    pub allow_task_reparenting: Option<bool>,
    pub test_only: Option<bool>,
    pub multi_arch: Option<bool>,
    /// `<supports-screens>` is a `<manifest>` child, but AOSP stores the parsed
    /// density/screen flags in `ApplicationInfo`; keeping it here also keeps
    /// `ApplicationInfo`'s `Default` the single "all attributes absent" value.
    pub supports_screens: SupportsScreens,
    pub theme: Option<u32>,
    pub process: Option<String>,
    /// `android:enabled` on `<application>`: `false` disables every component
    /// (``PackageParser.parseBaseApplication`` -> `ApplicationInfo.enabled`;
    /// `false` means none of them can be the launcher entry).
    pub enabled: Option<bool>,
    /// `<meta-data>` children of `<application>`, in declaration order
    /// (`PackageParser.mAppMetaData`).
    pub meta_data: Vec<MetaDataEntry>,
}

/// The kind of a manifest component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    Activity,
    /// `<activity-alias>`: a launcher entry that points at an `<activity>`
    /// through `android:targetActivity`. `PackageParser.parseActivityAlias`
    /// stores it in the same activity list as a plain `<activity>`, which is
    /// why it lives in [`Manifest::activities`] here.
    ActivityAlias,
    Service,
    Receiver,
    Provider,
}

/// A `<data>` element inside an `<intent-filter>`.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
pub struct IntentData {
    pub scheme: Option<String>,
    pub host: Option<String>,
    pub port: Option<String>,
    pub path: Option<String>,
    pub path_prefix: Option<String>,
    pub path_pattern: Option<String>,
    pub mime_type: Option<String>,
}

/// An `<intent-filter>`.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
pub struct IntentFilter {
    pub actions: Vec<String>,
    pub categories: Vec<String>,
    pub data: Vec<IntentData>,
}

impl IntentFilter {
    pub fn has_action(&self, action: &str) -> bool {
        self.actions.iter().any(|a| a == action)
    }

    pub fn has_category(&self, category: &str) -> bool {
        self.categories.iter().any(|c| c == category)
    }

    pub fn is_launcher(&self) -> bool {
        self.has_action("android.intent.action.MAIN")
            && self.has_category("android.intent.category.LAUNCHER")
    }
}

/// An activity/service/receiver/provider declaration.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Component {
    pub kind: ComponentKind,
    pub name: String,
    /// `android:authorities` (`<provider>` only), split on `;`.
    pub authorities: Vec<String>,
    pub exported: Option<bool>,
    pub launch_mode: Option<String>,
    pub process: Option<String>,
    pub permission: Option<String>,
    /// `android:grantUriPermissions` (`<provider>` only); `None` when absent,
    /// which is the platform default `false`.
    pub grant_uri_permissions: Option<bool>,
    pub label: Option<String>,
    pub label_ref: Option<u32>,
    pub intent_filters: Vec<IntentFilter>,
    /// `<activity-alias android:targetActivity>` as a fully qualified class
    /// name (`ActivityInfo.targetActivity`, which `ActivityThread` loads);
    /// `None` for a plain `<activity>` and for a `service`/`receiver`/`provider`.
    pub target_activity: Option<String>,
    /// `android:enabled` on the component; `None` when absent (default `true`).
    pub enabled: Option<bool>,
    /// `<meta-data>` children of this component (`PackageParser`'s
    /// `Component.metaData`, which `generateProviderInfo`/`generateActivityInfo`
    /// copy into the parcelable `metaData` field only under `GET_META_DATA`).
    pub meta_data: Vec<MetaDataEntry>,
}

/// The typed `AndroidManifest.xml` model.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Manifest {
    pub package: String,
    pub version_code: Option<i64>,
    pub version_code_major: Option<i32>,
    pub version_name: Option<String>,
    pub min_sdk: Option<u32>,
    pub target_sdk: Option<u32>,
    pub max_sdk: Option<u32>,
    pub compile_sdk: Option<u32>,
    pub uses_permissions: Vec<UsesPermission>,
    pub uses_features: Vec<UsesFeature>,
    /// Every `<permission>` the package itself declares, in manifest order.
    pub permissions: Vec<DeclaredPermission>,
    pub application: ApplicationInfo,
    pub activities: Vec<Component>,
    pub services: Vec<Component>,
    pub receivers: Vec<Component>,
    pub providers: Vec<Component>,
}

impl Manifest {
    /// Build the typed model from an AXML DOM.
    pub fn parse(doc: &XmlDocument) -> Result<Manifest> {
        let root = &doc.root;
        let package = attr_string(root, "package").unwrap_or_default();
        let version_code = attr_i64(root, "versionCode");
        let version_code_major = attr_i32(root, "versionCodeMajor");
        let version_name = attr_string(root, "versionName");
        let compile_sdk = attr_u32(root, "compileSdkVersion");

        let mut min_sdk = None;
        let mut target_sdk = None;
        let mut max_sdk = None;
        let mut uses_permissions = Vec::new();
        let mut uses_features = Vec::new();
        let mut permissions = Vec::new();
        let mut application = ApplicationInfo::default();
        let mut supports_screens = SupportsScreens::default();

        for child in &root.children {
            match child.name.as_str() {
                "uses-sdk" => {
                    min_sdk = attr_u32(child, "minSdkVersion");
                    target_sdk = attr_u32(child, "targetSdkVersion");
                    max_sdk = attr_u32(child, "maxSdkVersion");
                }
                "uses-permission" | "uses-permission-sdk-23" => {
                    if let Some(name) = attr_string(child, "name") {
                        uses_permissions.push(UsesPermission {
                            name,
                            max_sdk: attr_u32(child, "maxSdkVersion"),
                        });
                    }
                }
                "uses-feature" => {
                    if let Some(name) = attr_string(child, "name") {
                        uses_features.push(UsesFeature {
                            name,
                            required: attr_bool(child, "required"),
                            version: attr_u32(child, "version"),
                        });
                    }
                }
                "permission" => {
                    if let Some(name) = attr_string(child, "name") {
                        // `PackageParser.parsePermissionName`: a bare name is
                        // prefixed with the package, a leading dot is resolved
                        // against it; a fully qualified name is kept.
                        permissions.push(DeclaredPermission {
                            name: fq_name(&package, &name),
                            label: attr_string(child, "label"),
                            description: attr_string(child, "description"),
                            group: attr_string(child, "permissionGroup"),
                            protection_level: attr_string(child, "protectionLevel"),
                            background_permission: attr_string(child, "backgroundPermission"),
                        });
                    }
                }
                "supports-screens" => parse_supports_screens(child, &mut supports_screens),
                "application" => {
                    application = parse_application(child, &package);
                }
                _ => {}
            }
        }

        let (activities, services, receivers, providers) =
            match root.children_named("application").next() {
                Some(app) => (
                    parse_activities(app, &package),
                    parse_components(app, ComponentKind::Service, &package),
                    parse_components(app, ComponentKind::Receiver, &package),
                    parse_components(app, ComponentKind::Provider, &package),
                ),
                None => (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
            };

        // `<supports-screens>` may appear before or after `<application>`; the
        // typed model keeps the parsed group on `ApplicationInfo`.
        application.supports_screens = supports_screens;

        Ok(Manifest {
            package,
            version_code,
            version_code_major,
            version_name,
            min_sdk,
            target_sdk,
            max_sdk,
            compile_sdk,
            uses_permissions,
            uses_features,
            permissions,
            application,
            activities,
            services,
            receivers,
            providers,
        })
    }

    /// The `MAIN`/`LAUNCHER` component, if declared.
    ///
    /// Candidates are `<activity>` **and** `<activity-alias>` declarations whose
    /// `<intent-filter>` names both `android.intent.action.MAIN` and
    /// `android.intent.category.LAUNCHER` — the same set
    /// `PackageParser.parseBaseApplication` puts in `mActivities`. Gaten is the
    /// reason this exists: its launcher entries are `<activity-alias>` elements
    /// (`LauncherIconLight`/`LauncherIconSystem`/`LauncherIconDark`) targeting
    /// `LauncherActivity`, so a scan of `<activity>` alone finds no launcher and
    /// `acos inspect` reported `launcher_activity: null`.
    ///
    /// Resolution is deterministic and matches `PackageParser` +
    /// `LauncherApps` semantics:
    ///
    /// 1. a component with `android:enabled="false"` (or an application with
    ///    `android:enabled="false"`) is never the entry;
    /// 2. enabled aliases come first, then enabled activities — an alias exists
    ///    specifically to be the launcher entry (themed icons), and the target
    ///    activity usually carries no launcher filter at all;
    /// 3. within a group, manifest declaration order wins.
    ///
    /// The returned component is the one a launcher `Intent` names: for an
    /// alias that is the **alias**, and `target_activity` holds the class the
    /// framework actually instantiates.
    pub fn launcher_activity(&self) -> Option<&Component> {
        let app_enabled = self.application.enabled.unwrap_or(true);
        let enabled = |c: &&Component| app_enabled && c.enabled.unwrap_or(true);
        let launchers = || {
            self.activities
                .iter()
                .filter(|c| c.intent_filters.iter().any(IntentFilter::is_launcher))
        };
        launchers()
            .filter(|c| c.kind == ComponentKind::ActivityAlias && enabled(&c))
            .chain(launchers().filter(|c| c.kind == ComponentKind::Activity && enabled(&c)))
            .next()
    }

    /// The resolved application label (literal first, then resource lookup).
    pub fn app_label(&self, res: Option<&ResourceTable>) -> Option<String> {
        self.application.label.clone().or_else(|| {
            self.application
                .label_ref
                .and_then(|id| res.and_then(|r| r.resolve_string(id)))
        })
    }

    /// The resolved label of a component (falls back to nothing).
    pub fn component_label(&self, c: &Component, res: Option<&ResourceTable>) -> Option<String> {
        c.label.clone().or_else(|| {
            c.label_ref
                .and_then(|id| res.and_then(|r| r.resolve_string(id)))
        })
    }

    /// The `<application>` `ApplicationInfo.flags` **as `PackageParser` computes
    /// them**, transcribed from `.cache/aosp/base/core/java/android/content/pm/
    /// PackageParser.java::parseBaseApplication` (tag android-14.0.0_r75) and
    /// checked against the API-34 image's `ApplicationInfo` constants.
    ///
    /// `FLAG_INSTALLED` is **not** a parser flag: the package manager sets it on
    /// an installed package after parsing, so it is applied separately by
    /// [`Self::application_flags_installed`].
    ///
    /// Attributes this model does not carry yet (backupAgent, `killAfterRestore`,
    /// `restoreAnyVersion`, `fullBackupOnly`) are documented as unmodelled: their
    /// bits are only set when a backup agent is declared, and no corpus APK
    /// declares one.
    #[must_use]
    pub fn application_flags(&self) -> i32 {
        // `usesCleartextTraffic` and `hardwareAccelerated` default from
        // targetSdkVersion, so an absent `<uses-sdk>` keeps the launcher's
        // previous assumption (API 34).
        let target_sdk = self.target_sdk.unwrap_or(34) as i32;
        let app = &self.application;
        let mut flags = 0i32;
        // PackageParser.parseBaseApplication, in its own order.
        if app.allow_backup.unwrap_or(true) {
            flags |= FLAG_ALLOW_BACKUP;
        }
        if app.debuggable.unwrap_or(false) {
            flags |= FLAG_DEBUGGABLE;
        }
        if app.vm_safe_mode.unwrap_or(false) {
            flags |= FLAG_VM_SAFE_MODE;
        }
        if app.hardware_accelerated.unwrap_or(target_sdk >= 14) {
            flags |= FLAG_HARDWARE_ACCELERATED;
        }
        if app.has_code.unwrap_or(true) {
            flags |= FLAG_HAS_CODE;
        }
        if app.allow_task_reparenting.unwrap_or(false) {
            flags |= FLAG_ALLOW_TASK_REPARENTING;
        }
        if app.allow_clear_user_data.unwrap_or(true) {
            flags |= FLAG_ALLOW_CLEAR_USER_DATA;
        }
        if app.test_only.unwrap_or(false) {
            flags |= FLAG_TEST_ONLY;
        }
        if app.large_heap.unwrap_or(false) {
            flags |= FLAG_LARGE_HEAP;
        }
        // `sa.getBoolean(usesCleartextTraffic, targetSdkVersion < P)`: an app
        // targeting API 27 or lower may talk cleartext by default; an app
        // targeting 28+ must ask for `android:usesCleartextTraffic="true"`.
        if app.uses_cleartext_traffic.unwrap_or(target_sdk < 28) {
            flags |= FLAG_USES_CLEARTEXT_TRAFFIC;
        }
        if app.supports_rtl.unwrap_or(false) {
            flags |= FLAG_SUPPORTS_RTL;
        }
        if app.multi_arch.unwrap_or(false) {
            flags |= FLAG_MULTIARCH;
        }
        if app.extract_native_libs.unwrap_or(true) {
            flags |= FLAG_EXTRACT_NATIVE_LIBS;
        }
        if app.is_game.unwrap_or(false) {
            flags |= FLAG_IS_GAME;
        }
        // `<supports-screens>` (`PackageParser.parseBaseApkCommon` r75:2476-2503).
        // Screen flags use the parser's effective target SDK
        // (`targetSdkVersion`, defaulting to `minSdkVersion` inside
        // `<uses-sdk>` and to 0 when `<uses-sdk>` is absent), NOT the API-34
        // fallback used by unrelated application attributes above.
        let screen_sdk = self.target_sdk.or(self.min_sdk).unwrap_or(0) as i32;
        let screens = &app.supports_screens;
        if screen_flag(screens.small_screens, screen_sdk, Some(4)) {
            flags |= FLAG_SUPPORTS_SMALL_SCREENS;
        }
        if screen_flag(screens.normal_screens, screen_sdk, None) {
            flags |= FLAG_SUPPORTS_NORMAL_SCREENS;
        }
        if screen_flag(screens.large_screens, screen_sdk, Some(4)) {
            flags |= FLAG_SUPPORTS_LARGE_SCREENS;
        }
        if screen_flag(screens.xlarge_screens, screen_sdk, Some(9)) {
            flags |= FLAG_SUPPORTS_XLARGE_SCREENS;
        }
        if screen_flag(screens.resizeable, screen_sdk, Some(4)) {
            flags |= FLAG_RESIZEABLE_FOR_SCREENS;
        }
        if screen_flag(screens.any_density, screen_sdk, Some(4)) {
            flags |= FLAG_SUPPORTS_SCREEN_DENSITIES;
        }
        flags
    }

    /// The flags a **stock package manager leaves on an installed package**:
    /// the parser's flags plus `FLAG_INSTALLED` (`PackageManagerService`).
    #[must_use]
    pub fn application_flags_installed(&self) -> i32 {
        self.application_flags() | FLAG_INSTALLED
    }

    /// All components as `(kind, &Component)`.
    pub fn all_components(&self) -> Vec<(ComponentKind, &Component)> {
        let mut out = Vec::new();
        for c in &self.activities {
            out.push((c.kind, c));
        }
        for c in &self.services {
            out.push((ComponentKind::Service, c));
        }
        for c in &self.receivers {
            out.push((ComponentKind::Receiver, c));
        }
        for c in &self.providers {
            out.push((ComponentKind::Provider, c));
        }
        out
    }
}

fn parse_application(app: &XmlNode, package: &str) -> ApplicationInfo {
    let (label, label_ref) = label_of(app);
    ApplicationInfo {
        name: attr_string(app, "name").map(|n| fq_name(package, &n)),
        label,
        label_ref,
        debuggable: attr_bool(app, "debuggable"),
        allow_backup: attr_bool(app, "allowBackup"),
        extract_native_libs: attr_bool(app, "extractNativeLibs"),
        hardware_accelerated: attr_bool(app, "hardwareAccelerated"),
        enabled: attr_bool(app, "enabled"),
        has_code: attr_bool(app, "hasCode"),
        uses_cleartext_traffic: attr_bool(app, "usesCleartextTraffic"),
        large_heap: attr_bool(app, "largeHeap"),
        supports_rtl: attr_bool(app, "supportsRtl"),
        is_game: attr_bool(app, "isGame"),
        vm_safe_mode: attr_bool(app, "vmSafeMode"),
        allow_clear_user_data: attr_bool(app, "allowClearUserData"),
        allow_task_reparenting: attr_bool(app, "allowTaskReparenting"),
        test_only: attr_bool(app, "testOnly"),
        multi_arch: attr_bool(app, "multiArch"),
        supports_screens: SupportsScreens::default(),
        theme: attr_ref(app, "theme"),
        process: attr_string(app, "process"),
        meta_data: parse_meta_data(app),
    }
}

/// `v < 0 || (v > 0 && sdk >= gate)`, with `getInteger`'s absent sentinel
/// `1` for `None`. `gate == None` is the ungated `normalScreens != 0` case.
/// An unresolved reference is conservative: keep the bit clear rather than
/// granting density support the app did not literally ask for.
fn screen_flag(value: Option<SupportsScreensValue>, screen_sdk: i32, gate: Option<i32>) -> bool {
    let gated = gate.map_or(true, |threshold| screen_sdk >= threshold);
    match value {
        None => gated,
        Some(SupportsScreensValue::True) => true,
        Some(SupportsScreensValue::False) => false,
        Some(SupportsScreensValue::Reference(_)) => false,
    }
}

fn parse_supports_screens(node: &XmlNode, screens: &mut SupportsScreens) {
    // `sa.getInteger(attr, current)` keeps the current value for an absent
    // attribute, so later `<supports-screens>` tags merge into earlier ones
    // instead of resetting the group.
    if let Some(value) = parse_screen_value(node, "smallScreens") {
        screens.small_screens = Some(value);
    }
    if let Some(value) = parse_screen_value(node, "normalScreens") {
        screens.normal_screens = Some(value);
    }
    if let Some(value) = parse_screen_value(node, "largeScreens") {
        screens.large_screens = Some(value);
    }
    if let Some(value) = parse_screen_value(node, "xlargeScreens") {
        screens.xlarge_screens = Some(value);
    }
    if let Some(value) = parse_screen_value(node, "resizeable") {
        screens.resizeable = Some(value);
    }
    if let Some(value) = parse_screen_value(node, "anyDensity") {
        screens.any_density = Some(value);
    }
}

/// Our AXML decoder collapses `TYPE_INT_BOOLEAN` to a bool, and aapt2 encodes
/// `true` as `-1`, so `Boolean(true)` is the bypassing `True` state. Stock
/// resolves `@bool` references before `getInteger`; ACOS has no resource table
/// here, so references are recorded as unsupported rather than resolved.
/// Malformed boolean input is likewise left unsupported.
fn parse_screen_value(node: &XmlNode, name: &str) -> Option<SupportsScreensValue> {
    match &node.attribute(name)?.value {
        TypedValue::Boolean(true) => Some(SupportsScreensValue::True),
        TypedValue::Boolean(false) => Some(SupportsScreensValue::False),
        TypedValue::Reference(id) => Some(SupportsScreensValue::Reference(*id)),
        _ => None,
    }
}

/// `<activity>` and `<activity-alias>` declarations, in manifest order.
///
/// An alias is kept in the activity list (§`PackageParser.parseActivityAlias`
/// adds it to `owner.activities`) and carries its `targetActivity`, so the
/// launcher filter on an alias is visible to [`Manifest::launcher_activity`].
fn parse_activities(parent: &XmlNode, package: &str) -> Vec<Component> {
    let mut out = Vec::new();
    for node in &parent.children {
        let kind = match node.name.as_str() {
            "activity" => ComponentKind::Activity,
            "activity-alias" => ComponentKind::ActivityAlias,
            _ => continue,
        };
        if let Some(component) = parse_component(node, kind, package) {
            out.push(component);
        }
    }
    out
}

fn parse_components(parent: &XmlNode, kind: ComponentKind, package: &str) -> Vec<Component> {
    let tag = match kind {
        ComponentKind::Activity | ComponentKind::ActivityAlias => "activity",
        ComponentKind::Service => "service",
        ComponentKind::Receiver => "receiver",
        ComponentKind::Provider => "provider",
    };
    let mut out = Vec::new();
    for node in parent.children_named(tag) {
        if let Some(component) = parse_component(node, kind, package) {
            out.push(component);
        }
    }
    out
}

fn parse_component(node: &XmlNode, kind: ComponentKind, package: &str) -> Option<Component> {
    let raw_name = attr_string(node, "name").unwrap_or_default();
    if raw_name.is_empty() {
        return None;
    }
    let (label, label_ref) = label_of(node);
    let mut intent_filters = Vec::new();
    for f in node.children_named("intent-filter") {
        intent_filters.push(parse_intent_filter(f));
    }
    let authorities = attr_string(node, "authorities")
        .map(|value| {
            value
                .split(';')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(Component {
        kind,
        name: fq_name(package, &raw_name),
        authorities,
        exported: attr_bool(node, "exported"),
        launch_mode: attr_string(node, "launchMode"),
        process: attr_string(node, "process"),
        permission: attr_string(node, "permission"),
        grant_uri_permissions: attr_bool(node, "grantUriPermissions"),
        label,
        label_ref,
        intent_filters,
        target_activity: attr_string(node, "targetActivity").map(|t| fq_name(package, &t)),
        enabled: attr_bool(node, "enabled"),
        meta_data: parse_meta_data(node),
    })
}

/// `<meta-data>` children of a node, as `PackageParser.parseMetaData` stores
/// them. An entry without `android:name` is skipped (stock rejects the manifest;
/// ACOS keeps the rest of the declaration rather than failing the whole parse),
/// and an entry whose value type the platform does not support is not stored
/// (`parseMetaData` logs it when `RIGID_PARSER` is off).
fn parse_meta_data(node: &XmlNode) -> Vec<MetaDataEntry> {
    let mut out = Vec::new();
    for md in node.children_named("meta-data") {
        let Some(name) = attr_string(md, "name") else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let Some(value) = meta_data_value(md) else {
            continue;
        };
        out.push(MetaDataEntry { name, value });
    }
    out
}

fn meta_data_value(node: &XmlNode) -> Option<MetaDataValue> {
    // `parseMetaData` reads `android:resource` first and prefers it whenever
    // the typed value carries a non-zero resource id.
    if let Some(TypedValue::Reference(id)) = node.attribute("resource").map(|a| &a.value) {
        if *id != 0 {
            return Some(MetaDataValue::Reference(*id));
        }
    }
    match node.attribute("value").map(|a| &a.value)? {
        TypedValue::String(text) => Some(MetaDataValue::String(text.clone())),
        TypedValue::Boolean(flag) => Some(MetaDataValue::Boolean(*flag)),
        TypedValue::IntDec(value) => Some(MetaDataValue::Int(*value)),
        TypedValue::IntHex(value) => Some(MetaDataValue::Int(*value as i32)),
        TypedValue::ColorArgb8(colour)
        | TypedValue::ColorRgb8(colour)
        | TypedValue::ColorArgb4(colour)
        | TypedValue::ColorRgb4(colour) => Some(MetaDataValue::Int(*colour as i32)),
        TypedValue::Float(value) => Some(MetaDataValue::Float(*value)),
        // Everything else (including a reference in `android:value`) is not a
        // type `parseMetaData` stores.
        _ => None,
    }
}

fn parse_intent_filter(node: &XmlNode) -> IntentFilter {
    let mut filter = IntentFilter::default();
    for child in &node.children {
        match child.name.as_str() {
            "action" => {
                if let Some(name) = attr_string(child, "name") {
                    filter.actions.push(name);
                }
            }
            "category" => {
                if let Some(name) = attr_string(child, "name") {
                    filter.categories.push(name);
                }
            }
            "data" => {
                filter.data.push(IntentData {
                    scheme: attr_string(child, "scheme"),
                    host: attr_string(child, "host"),
                    port: attr_string(child, "port"),
                    path: attr_string(child, "path"),
                    path_prefix: attr_string(child, "pathPrefix"),
                    path_pattern: attr_string(child, "pathPattern"),
                    mime_type: attr_string(child, "mimeType"),
                });
            }
            _ => {}
        }
    }
    filter
}

/// Resolve a manifest component name to a fully-qualified name.
pub fn fq_name(package: &str, name: &str) -> String {
    if name.starts_with('.') {
        format!("{package}{name}")
    } else if !name.contains('.') && !package.is_empty() {
        format!("{package}.{name}")
    } else {
        name.to_string()
    }
}

fn attribute<'a>(node: &'a XmlNode, name: &str) -> Option<&'a XmlAttribute> {
    node.attribute(name)
}

fn attr_string(node: &XmlNode, name: &str) -> Option<String> {
    match &attribute(node, name)?.value {
        TypedValue::String(s) => Some(s.clone()),
        TypedValue::Boolean(b) => Some(b.to_string()),
        TypedValue::IntDec(i) => Some(i.to_string()),
        TypedValue::IntHex(h) => Some(format!("{h:#x}")),
        _ => None,
    }
}

fn attr_bool(node: &XmlNode, name: &str) -> Option<bool> {
    match &attribute(node, name)?.value {
        TypedValue::Boolean(b) => Some(*b),
        TypedValue::IntDec(i) => Some(*i != 0),
        _ => None,
    }
}

fn attr_u32(node: &XmlNode, name: &str) -> Option<u32> {
    match &attribute(node, name)?.value {
        TypedValue::IntDec(i) if *i >= 0 => Some(*i as u32),
        TypedValue::IntHex(h) => Some(*h),
        _ => None,
    }
}

fn attr_i32(node: &XmlNode, name: &str) -> Option<i32> {
    match &attribute(node, name)?.value {
        TypedValue::IntDec(i) => Some(*i),
        TypedValue::IntHex(h) => Some(*h as i32),
        _ => None,
    }
}

fn attr_i64(node: &XmlNode, name: &str) -> Option<i64> {
    match &attribute(node, name)?.value {
        TypedValue::IntDec(i) => Some(*i as i64),
        TypedValue::IntHex(h) => Some(*h as i64),
        _ => None,
    }
}

fn attr_ref(node: &XmlNode, name: &str) -> Option<u32> {
    match &attribute(node, name)?.value {
        TypedValue::Reference(r) => Some(*r),
        _ => None,
    }
}

fn label_of(node: &XmlNode) -> (Option<String>, Option<u32>) {
    match node.attribute("label") {
        Some(a) => match &a.value {
            TypedValue::String(s) => (Some(s.clone()), None),
            TypedValue::Reference(r) => (None, Some(*r)),
            other => (Some(other.display()), None),
        },
        None => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANDROID_NS: &str = "http://schemas.android.com/apk/res/android";

    pub(crate) fn attr(name: &str, value: TypedValue) -> XmlAttribute {
        XmlAttribute {
            namespace: Some(ANDROID_NS.to_string()),
            name: name.to_string(),
            raw_value: None,
            value,
            resource_id: None,
            name_index: 0,
        }
    }

    pub(crate) fn node(
        name: &str,
        attributes: Vec<XmlAttribute>,
        children: Vec<XmlNode>,
    ) -> XmlNode {
        XmlNode {
            namespace: None,
            name: name.to_string(),
            line: 0,
            attributes,
            children,
        }
    }

    fn meta(name: &str, attribute: XmlAttribute) -> XmlNode {
        node(
            "meta-data",
            vec![
                attr("name", TypedValue::String(name.to_string())),
                attribute,
            ],
            Vec::new(),
        )
    }

    fn value(v: TypedValue) -> XmlNode {
        meta("k", attr("value", v))
    }

    pub(crate) fn document(application_children: Vec<XmlNode>) -> XmlDocument {
        XmlDocument {
            strings: Vec::new(),
            resource_ids: Vec::new(),
            root: node(
                "manifest",
                vec![attr("package", TypedValue::String("pkg".into()))],
                vec![node(
                    "application",
                    vec![attr("name", TypedValue::String(".App".into()))],
                    application_children,
                )],
            ),
        }
    }

    /// A manifest with optional `<uses-sdk>` (min/target) and extra
    /// `<manifest>` children (`<supports-screens>`).
    fn document_with_sdk(
        min_sdk: Option<i32>,
        target_sdk: Option<i32>,
        manifest_children: Vec<XmlNode>,
    ) -> XmlDocument {
        let mut children = Vec::new();
        if min_sdk.is_some() || target_sdk.is_some() {
            let mut attrs = Vec::new();
            if let Some(api) = min_sdk {
                attrs.push(attr("minSdkVersion", TypedValue::IntDec(api)));
            }
            if let Some(api) = target_sdk {
                attrs.push(attr("targetSdkVersion", TypedValue::IntDec(api)));
            }
            children.push(node("uses-sdk", attrs, Vec::new()));
        }
        children.extend(manifest_children);
        children.push(node(
            "application",
            vec![attr("name", TypedValue::String(".App".into()))],
            Vec::new(),
        ));
        XmlDocument {
            strings: Vec::new(),
            resource_ids: Vec::new(),
            root: node(
                "manifest",
                vec![attr("package", TypedValue::String("pkg".into()))],
                children,
            ),
        }
    }

    fn document_with(target_sdk: Option<i32>, manifest_children: Vec<XmlNode>) -> XmlDocument {
        document_with_sdk(None, target_sdk, manifest_children)
    }

    /// SYSUI3B: `<permission>` declarations are part of the typed model, with
    /// the platform's name normalisation (`PackageParser.parsePermissionName`).
    #[test]
    fn a_declared_permission_keeps_its_label_group_and_protection_level() {
        let document = document_with_sdk(
            None,
            Some(34),
            vec![
                node(
                    "permission",
                    vec![
                        attr("name", TypedValue::String(".permission.PROBE".into())),
                        attr(
                            "label",
                            TypedValue::String("probe permission".into()),
                        ),
                        attr(
                            "description",
                            TypedValue::String("a declared permission".into()),
                        ),
                        attr(
                            "permissionGroup",
                            TypedValue::String("pkg.permission-group.PROBE".into()),
                        ),
                        attr(
                            "protectionLevel",
                            TypedValue::String("dangerous|instant".into()),
                        ),
                        attr(
                            "backgroundPermission",
                            TypedValue::String("pkg.permission.PROBE_BACKGROUND".into()),
                        ),
                    ],
                    Vec::new(),
                ),
                node(
                    "permission",
                    vec![
                        attr("name", TypedValue::String("Bare".into())),
                        attr("protectionLevel", TypedValue::String("signature".into())),
                    ],
                    Vec::new(),
                ),
            ],
        );
        let manifest = Manifest::parse(&document).expect("manifest parses");
        assert_eq!(manifest.permissions.len(), 2);
        let probe = &manifest.permissions[0];
        assert_eq!(probe.name, "pkg.permission.PROBE");
        assert_eq!(probe.label.as_deref(), Some("probe permission"));
        assert_eq!(probe.description.as_deref(), Some("a declared permission"));
        assert_eq!(probe.group.as_deref(), Some("pkg.permission-group.PROBE"));
        assert_eq!(probe.protection_level.as_deref(), Some("dangerous|instant"));
        assert_eq!(
            probe.background_permission.as_deref(),
            Some("pkg.permission.PROBE_BACKGROUND")
        );
        // A bare name gets the package prefix, and an absent label stays absent
        // (a `@string/...` reference would too: resolving it needs the app's
        // resource table).
        assert_eq!(manifest.permissions[1].name, "pkg.Bare");
        assert_eq!(manifest.permissions[1].label, None);
    }

    /// One `<activity>`/`<activity-alias>` with an optional MAIN/LAUNCHER
    /// `<intent-filter>` and an optional `android:enabled`.
    pub(crate) fn component_node(
        tag: &str,
        name: &str,
        target: Option<&str>,
        enabled: Option<bool>,
        launcher: bool,
    ) -> XmlNode {
        let mut attrs = vec![attr("name", TypedValue::String(name.to_string()))];
        if let Some(target) = target {
            attrs.push(attr(
                "targetActivity",
                TypedValue::String(target.to_string()),
            ));
        }
        if let Some(enabled) = enabled {
            attrs.push(attr("enabled", TypedValue::Boolean(enabled)));
        }
        let mut children = Vec::new();
        if launcher {
            children.push(node(
                "intent-filter",
                Vec::new(),
                vec![
                    node(
                        "action",
                        vec![attr(
                            "name",
                            TypedValue::String("android.intent.action.MAIN".to_string()),
                        )],
                        Vec::new(),
                    ),
                    node(
                        "category",
                        vec![attr(
                            "name",
                            TypedValue::String("android.intent.category.LAUNCHER".to_string()),
                        )],
                        Vec::new(),
                    ),
                ],
            ));
        }
        node(tag, attrs, children)
    }

    fn supports_screens(attributes: Vec<XmlAttribute>) -> XmlNode {
        node("supports-screens", attributes, Vec::new())
    }

    const ALL_SCREEN_FLAGS: i32 = FLAG_SUPPORTS_SMALL_SCREENS
        | FLAG_SUPPORTS_NORMAL_SCREENS
        | FLAG_SUPPORTS_LARGE_SCREENS
        | FLAG_RESIZEABLE_FOR_SCREENS
        | FLAG_SUPPORTS_SCREEN_DENSITIES
        | FLAG_SUPPORTS_XLARGE_SCREENS;

    fn screen_flags(manifest: &Manifest) -> i32 {
        manifest.application_flags() & ALL_SCREEN_FLAGS
    }

    /// `PackageParser.parseBaseApkCommon` (r75:1993-1998, 2476-2503) starts
    /// every value at the "unknown" sentinel `1`. The screen effective SDK is
    /// `targetSdkVersion`, defaulting to `minSdkVersion` inside `<uses-sdk>`
    /// and to `0` when `<uses-sdk>` is absent; it must not inherit the API-34
    /// fallback used by unrelated application attributes.
    #[test]
    fn supports_screens_absent_uses_screen_effective_sdk_defaults() {
        let cases = [
            // (min, target, expected) — no <uses-sdk> means SDK 0: only the
            // ungated normalScreens default survives.
            (None, None, FLAG_SUPPORTS_NORMAL_SCREENS),
            (Some(3), None, FLAG_SUPPORTS_NORMAL_SCREENS),
            (
                Some(4),
                None,
                ALL_SCREEN_FLAGS & !FLAG_SUPPORTS_XLARGE_SCREENS,
            ),
            (Some(9), None, ALL_SCREEN_FLAGS),
            (Some(26), Some(3), FLAG_SUPPORTS_NORMAL_SCREENS),
            (
                None,
                Some(8),
                ALL_SCREEN_FLAGS & !FLAG_SUPPORTS_XLARGE_SCREENS,
            ),
            (Some(26), Some(34), ALL_SCREEN_FLAGS),
        ];
        for (min_sdk, target_sdk, expected) in cases {
            let doc = document_with_sdk(min_sdk, target_sdk, Vec::new());
            let manifest = Manifest::parse(&doc).expect("manifest");
            assert_eq!(
                screen_flags(&manifest),
                expected,
                "minSdk {min_sdk:?}, targetSdk {target_sdk:?}"
            );
        }
    }

    /// Each explicit `false` clears exactly its own bit; all six false leave
    /// the compatibility mask empty. This is how an app intentionally opts out
    /// of a screen class or of density scaling (`anyDensity=false` is the
    /// legacy mode `ActivityThread` reads).
    #[test]
    fn supports_screens_explicit_false_clears_only_its_flag() {
        for (attribute, bit) in [
            ("smallScreens", FLAG_SUPPORTS_SMALL_SCREENS),
            ("normalScreens", FLAG_SUPPORTS_NORMAL_SCREENS),
            ("largeScreens", FLAG_SUPPORTS_LARGE_SCREENS),
            ("xlargeScreens", FLAG_SUPPORTS_XLARGE_SCREENS),
            ("resizeable", FLAG_RESIZEABLE_FOR_SCREENS),
            ("anyDensity", FLAG_SUPPORTS_SCREEN_DENSITIES),
        ] {
            let doc = document_with_sdk(
                None,
                Some(34),
                vec![supports_screens(vec![attr(
                    attribute,
                    TypedValue::Boolean(false),
                )])],
            );
            let manifest = Manifest::parse(&doc).expect("manifest");
            assert_eq!(
                screen_flags(&manifest),
                ALL_SCREEN_FLAGS & !bit,
                "{attribute}=false must clear only its bit"
            );
        }

        let doc = document_with_sdk(
            None,
            Some(34),
            vec![supports_screens(
                [
                    ("smallScreens", FLAG_SUPPORTS_SMALL_SCREENS),
                    ("normalScreens", FLAG_SUPPORTS_NORMAL_SCREENS),
                    ("largeScreens", FLAG_SUPPORTS_LARGE_SCREENS),
                    ("xlargeScreens", FLAG_SUPPORTS_XLARGE_SCREENS),
                    ("resizeable", FLAG_RESIZEABLE_FOR_SCREENS),
                    ("anyDensity", FLAG_SUPPORTS_SCREEN_DENSITIES),
                ]
                .into_iter()
                .map(|(name, _)| attr(name, TypedValue::Boolean(false)))
                .collect(),
            )],
        );
        let manifest = Manifest::parse(&doc).expect("manifest");
        assert_eq!(screen_flags(&manifest), 0, "all six explicit false");
    }

    /// aapt2 stores an explicit `true` as `-1`, which hits AOSP's `v < 0`
    /// branch and bypasses the DONUT/GINGERBREAD gates; an absent attribute
    /// (`1`) does not.
    #[test]
    fn supports_screens_explicit_true_bypasses_target_sdk_gates() {
        for (min_sdk, target_sdk) in [(None, None), (Some(1), Some(3)), (Some(26), Some(34))] {
            let attrs = [
                "smallScreens",
                "normalScreens",
                "largeScreens",
                "xlargeScreens",
                "resizeable",
                "anyDensity",
            ]
            .into_iter()
            .map(|name| attr(name, TypedValue::Boolean(true)))
            .collect();
            let doc = document_with_sdk(min_sdk, target_sdk, vec![supports_screens(attrs)]);
            let manifest = Manifest::parse(&doc).expect("manifest");
            assert_eq!(
                screen_flags(&manifest),
                ALL_SCREEN_FLAGS,
                "minSdk {min_sdk:?}, targetSdk {target_sdk:?}: explicit true bypasses gates"
            );
        }
    }

    /// A resource reference is recorded but cannot be resolved: ACOS has no
    /// `resources.arsc` handle in `Manifest::parse` / `application_flags`
    /// (plumbing it needs `Apk::manifest` + PMS changes, deferred), so the
    /// density bit stays clear rather than being granted by the
    /// absent-attribute default. Stock resolves `@bool` before `getInteger`;
    /// this under-grants `@bool/true` but matches `@bool/false`.
    #[test]
    fn supports_screens_reference_is_recorded_and_not_granted() {
        let doc = document_with_sdk(
            None,
            Some(34),
            vec![supports_screens(vec![attr(
                "anyDensity",
                TypedValue::Reference(0x7f05_0001),
            )])],
        );
        let manifest = Manifest::parse(&doc).expect("manifest");
        assert_eq!(
            manifest.application.supports_screens.any_density,
            Some(SupportsScreensValue::Reference(0x7f05_0001))
        );
        assert_eq!(
            manifest.application_flags() & FLAG_SUPPORTS_SCREEN_DENSITIES,
            0,
            "an unresolved @bool reference must not grant density support"
        );
        assert_eq!(
            screen_flags(&manifest),
            ALL_SCREEN_FLAGS & !FLAG_SUPPORTS_SCREEN_DENSITIES,
            "the other absent defaults are unaffected"
        );
    }

    /// Every typed value kind `PackageParser.parseMetaData` stores, exactly as
    /// it stores it (`putString`/`putInt`/`putBoolean`/`putFloat`, and the
    /// `android:resource` reference as `putInt(name, resourceId)`).
    #[test]
    fn meta_data_typed_values_follow_parse_meta_data() {
        let doc = document(vec![
            value(TypedValue::String("text".into())),
            value(TypedValue::IntDec(-7)),
            value(TypedValue::IntHex(0x7f0b_0001)),
            value(TypedValue::Boolean(true)),
            value(TypedValue::Float(1.5)),
            value(TypedValue::ColorArgb8(0xffff_0000)),
            value(TypedValue::ColorRgb4(0x0f00)),
            meta("res", attr("resource", TypedValue::Reference(0x7f0b_0002))),
        ]);
        let manifest = Manifest::parse(&doc).expect("manifest");
        let entries = &manifest.application.meta_data;
        assert_eq!(
            entries
                .iter()
                .map(|e| e.value.clone())
                .collect::<Vec<MetaDataValue>>(),
            vec![
                MetaDataValue::String("text".into()),
                MetaDataValue::Int(-7),
                MetaDataValue::Int(0x7f0b_0001),
                MetaDataValue::Boolean(true),
                MetaDataValue::Float(1.5),
                MetaDataValue::Int(0xffff_0000u32 as i32),
                MetaDataValue::Int(0x0f00),
                MetaDataValue::Reference(0x7f0b_0002),
            ],
            "typed values in declaration order"
        );
        assert!(entries.iter().all(|e| e.name == "k" || e.name == "res"));
    }

    /// `android:resource` wins over `android:value` (`parseMetaData` peeks the
    /// reference first), a reference in `android:value` is not a stored type,
    /// and an entry with neither attribute is dropped.
    #[test]
    fn meta_data_reference_rules_match_parse_meta_data() {
        let doc = document(vec![
            node(
                "meta-data",
                vec![
                    attr("name", TypedValue::String("both".into())),
                    attr("value", TypedValue::String("ignored".into())),
                    attr("resource", TypedValue::Reference(0x7f01_0000)),
                ],
                Vec::new(),
            ),
            value(TypedValue::Reference(0x7f01_0001)),
            node(
                "meta-data",
                vec![attr("name", TypedValue::String("empty".into()))],
                Vec::new(),
            ),
            node(
                "meta-data",
                vec![attr("value", TypedValue::IntDec(1))],
                Vec::new(),
            ),
        ]);
        let manifest = Manifest::parse(&doc).expect("manifest");
        assert_eq!(
            manifest.application.meta_data,
            vec![MetaDataEntry {
                name: "both".into(),
                value: MetaDataValue::Reference(0x7f01_0000),
            }],
            "resource wins; a reference in android:value is not stored; \
             nameless and valueless entries are dropped"
        );
    }

    /// Components carry their own `<meta-data>` (`Component.metaData`), which is
    /// what PMS copies into `ProviderInfo`/`ServiceInfo`/`ActivityInfo` only
    /// when `GET_META_DATA` is requested.
    #[test]
    fn component_meta_data_is_carried_per_component() {
        let doc = XmlDocument {
            strings: Vec::new(),
            resource_ids: Vec::new(),
            root: node(
                "manifest",
                vec![attr("package", TypedValue::String("pkg".into()))],
                vec![node(
                    "application",
                    Vec::new(),
                    vec![
                        node(
                            "provider",
                            vec![
                                attr("name", TypedValue::String(".P".into())),
                                attr("authorities", TypedValue::String("pkg".into())),
                            ],
                            vec![meta(
                                "android.support.FILE_PROVIDER_PATHS",
                                attr("resource", TypedValue::Reference(0x7f0b_0000)),
                            )],
                        ),
                        node(
                            "service",
                            vec![attr("name", TypedValue::String(".S".into()))],
                            vec![value(TypedValue::Boolean(false))],
                        ),
                        node(
                            "activity",
                            vec![attr("name", TypedValue::String(".A".into()))],
                            vec![value(TypedValue::String("shortcut".into()))],
                        ),
                    ],
                )],
            ),
        };
        let manifest = Manifest::parse(&doc).expect("manifest");
        assert!(manifest.application.meta_data.is_empty());
        assert_eq!(
            manifest.providers[0].meta_data,
            vec![MetaDataEntry {
                name: "android.support.FILE_PROVIDER_PATHS".into(),
                value: MetaDataValue::Reference(0x7f0b_0000),
            }]
        );
        assert_eq!(
            manifest.services[0].meta_data,
            vec![MetaDataEntry {
                name: "k".into(),
                value: MetaDataValue::Boolean(false),
            }]
        );
        assert_eq!(
            manifest.activities[0].meta_data,
            vec![MetaDataEntry {
                name: "k".into(),
                value: MetaDataValue::String("shortcut".into()),
            }]
        );
    }

    /// A real corpus APK: `org.billthefarmer.editor` declares
    /// `<provider android:name="androidx.core.content.FileProvider">` with
    /// `android.support.FILE_PROVIDER_PATHS` = `@xml/…` (aapt2 dump xmltree,
    /// build-tools 35.0.0). FileProvider resolves exactly that entry through
    /// `resolveContentProvider(authority, GET_META_DATA)`.
    #[test]
    fn corpus_editor_provider_meta_data_is_a_reference() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.cache/corpus/k1/apks/org.billthefarmer.editor_196_1.96.apk");
        if acos_test_support::require_prereq(
            path.is_file(),
            "corpus_editor_provider_meta_data_is_a_reference",
            &format!(
                "{} is absent — put the corpus APK under .cache/corpus/k1/apks",
                path.display()
            ),
        )
        .is_none()
        {
            return;
        }
        let apk = crate::Apk::open(&path).expect("open editor APK");
        let manifest = apk.manifest().expect("parse editor manifest");
        let provider = manifest
            .providers
            .iter()
            .find(|c| c.name.ends_with("FileProvider"))
            .expect("editor declares androidx FileProvider");
        let entry = provider
            .meta_data
            .iter()
            .find(|e| e.name == "android.support.FILE_PROVIDER_PATHS")
            .expect("FILE_PROVIDER_PATHS metadata");
        match entry.value {
            MetaDataValue::Reference(id) => assert_ne!(id, 0, "a resolved @xml resource id"),
            ref other => panic!("expected a resource reference, got {other:?}"),
        }
    }
}

/// F6: intent filters on `<activity-alias>` (Gaten's launcher entries) must be
/// parsed, reported, and used to resolve `launcher_activity`.
#[cfg(test)]
mod launcher_tests {
    use super::tests::*;
    use super::*;

    /// Gaten's real shape: three themed aliases target one `LauncherActivity`,
    /// only the light one is enabled, and no `<activity>` carries a launcher
    /// filter at all. The alias is the component a launcher Intent names.
    #[test]
    fn enabled_alias_is_the_launcher_and_filters_are_reported() {
        let dom = document(vec![
            component_node("activity", ".MainActivity", None, None, false),
            component_node("activity", ".LauncherActivity", None, None, false),
            component_node(
                "activity-alias",
                ".LauncherIconSystem",
                Some(".LauncherActivity"),
                Some(false),
                true,
            ),
            component_node(
                "activity-alias",
                ".LauncherIconLight",
                Some(".LauncherActivity"),
                Some(true),
                true,
            ),
            component_node(
                "activity-alias",
                ".LauncherIconDark",
                Some(".LauncherActivity"),
                Some(false),
                true,
            ),
        ]);
        let manifest = Manifest::parse(&dom).expect("manifest");
        // Aliases live in the activity list, in declaration order (AOSP
        // `parseActivityAlias` adds them to `owner.activities`).
        assert_eq!(manifest.activities.len(), 5);
        let launcher = manifest.launcher_activity().expect("a launcher");
        assert_eq!(launcher.name, "pkg.LauncherIconLight");
        assert_eq!(launcher.kind, ComponentKind::ActivityAlias);
        assert_eq!(
            launcher.target_activity.as_deref(),
            Some("pkg.LauncherActivity")
        );
        // The filter that made it a launcher is reported, not dropped.
        assert_eq!(launcher.intent_filters.len(), 1);
        assert!(launcher.intent_filters[0].is_launcher());
        // The disabled aliases still carry their filters (an agent must be able
        // to see them), they just are not the entry.
        for alias in ["LauncherIconSystem", "LauncherIconDark"] {
            let component = manifest
                .activities
                .iter()
                .find(|c| c.name == format!("pkg.{alias}"))
                .expect("alias");
            assert_eq!(component.enabled, Some(false));
            assert!(component.intent_filters[0].is_launcher());
        }
        // The plain activities have no filter and are still declared.
        let main = manifest
            .activities
            .iter()
            .find(|c| c.name == "pkg.MainActivity")
            .expect("MainActivity");
        assert!(main.intent_filters.is_empty());
        assert_eq!(main.target_activity, None);
    }

    /// With no alias in play, a plain launcher `<activity>` still resolves.
    #[test]
    fn plain_launcher_activity_still_resolves() {
        let dom = document(vec![
            component_node("activity", ".OtherActivity", None, None, false),
            component_node("activity", ".MainActivity", None, None, true),
        ]);
        let manifest = Manifest::parse(&dom).expect("manifest");
        let launcher = manifest.launcher_activity().expect("launcher");
        assert_eq!(launcher.name, "pkg.MainActivity");
        assert_eq!(launcher.kind, ComponentKind::Activity);
    }

    /// A disabled activity is not the entry; a disabled `<application>` makes
    /// every component ineligible.
    #[test]
    fn disabled_components_are_never_the_launcher() {
        let dom = document(vec![component_node(
            "activity",
            ".MainActivity",
            None,
            Some(false),
            true,
        )]);
        let manifest = Manifest::parse(&dom).expect("manifest");
        assert!(manifest.launcher_activity().is_none());

        let dom = document(vec![
            component_node("activity", ".MainActivity", None, None, true),
            component_node(
                "activity-alias",
                ".Alias",
                Some(".MainActivity"),
                None,
                true,
            ),
        ]);
        let mut manifest = Manifest::parse(&dom).expect("manifest");
        // Enabled alias wins over the enabled activity (declaration order is
        // within a group, not across the alias/activity groups).
        assert_eq!(
            manifest.launcher_activity().map(|c| c.name.clone()),
            Some("pkg.Alias".to_string())
        );
        manifest.application.enabled = Some(false);
        assert!(
            manifest.launcher_activity().is_none(),
            "<application android:enabled=\"false\"> disables every component"
        );
    }
}
