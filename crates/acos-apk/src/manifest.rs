//! The typed Android manifest model, built on the AXML DOM.

use crate::arsc::ResourceTable;
use crate::axml::{TypedValue, XmlAttribute, XmlDocument, XmlNode};
use crate::error::Result;

// `ApplicationInfo` flag bits, from the API-34 image's `ApplicationInfo`
// (`.cache/aosp/base/core/java/android/content/pm/ApplicationInfo.java`).
pub const FLAG_DEBUGGABLE: i32 = 1 << 1;
pub const FLAG_HAS_CODE: i32 = 1 << 2;
pub const FLAG_ALLOW_TASK_REPARENTING: i32 = 1 << 5;
pub const FLAG_TEST_ONLY: i32 = 1 << 8;
pub const FLAG_ALLOW_CLEAR_USER_DATA: i32 = 1 << 6;
pub const FLAG_VM_SAFE_MODE: i32 = 1 << 14;
pub const FLAG_ALLOW_BACKUP: i32 = 1 << 15;
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
    pub theme: Option<u32>,
    pub process: Option<String>,
    /// `<meta-data>` children of `<application>`, in declaration order
    /// (`PackageParser.mAppMetaData`).
    pub meta_data: Vec<MetaDataEntry>,
}

/// The kind of a manifest component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ComponentKind {
    Activity,
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
        let mut application = ApplicationInfo::default();

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
                "application" => {
                    application = parse_application(child, &package);
                }
                _ => {}
            }
        }

        let (activities, services, receivers, providers) =
            match root.children_named("application").next() {
                Some(app) => (
                    parse_components(app, ComponentKind::Activity, &package),
                    parse_components(app, ComponentKind::Service, &package),
                    parse_components(app, ComponentKind::Receiver, &package),
                    parse_components(app, ComponentKind::Provider, &package),
                ),
                None => (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
            };

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
            application,
            activities,
            services,
            receivers,
            providers,
        })
    }

    /// The `MAIN`/`LAUNCHER` activity, if declared.
    pub fn launcher_activity(&self) -> Option<&Component> {
        self.activities
            .iter()
            .find(|c| c.intent_filters.iter().any(IntentFilter::is_launcher))
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
            out.push((ComponentKind::Activity, c));
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
        theme: attr_ref(app, "theme"),
        process: attr_string(app, "process"),
        meta_data: parse_meta_data(app),
    }
}

fn parse_components(parent: &XmlNode, kind: ComponentKind, package: &str) -> Vec<Component> {
    let tag = match kind {
        ComponentKind::Activity => "activity",
        ComponentKind::Service => "service",
        ComponentKind::Receiver => "receiver",
        ComponentKind::Provider => "provider",
    };
    let mut out = Vec::new();
    for node in parent.children_named(tag) {
        let raw_name = attr_string(node, "name").unwrap_or_default();
        if raw_name.is_empty() {
            continue;
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
        out.push(Component {
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
            meta_data: parse_meta_data(node),
        });
    }
    out
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

    fn attr(name: &str, value: TypedValue) -> XmlAttribute {
        XmlAttribute {
            namespace: Some(ANDROID_NS.to_string()),
            name: name.to_string(),
            raw_value: None,
            value,
            resource_id: None,
            name_index: 0,
        }
    }

    fn node(name: &str, attributes: Vec<XmlAttribute>, children: Vec<XmlNode>) -> XmlNode {
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
            vec![attr("name", TypedValue::String(name.to_string())), attribute],
            Vec::new(),
        )
    }

    fn value(v: TypedValue) -> XmlNode {
        meta("k", attr("value", v))
    }

    fn document(application_children: Vec<XmlNode>) -> XmlDocument {
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
            node("meta-data", vec![attr("value", TypedValue::IntDec(1))], Vec::new()),
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
        if !path.is_file() {
            eprintln!("skip corpus_editor_provider_meta_data_is_a_reference: corpus not present");
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
