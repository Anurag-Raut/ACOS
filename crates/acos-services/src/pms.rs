//! Mini `PackageManagerService` built from the fixture APK.
//!
//! The typed values come from `acos-apk`'s manifest parser plus the ACOS VFS
//! layout (`uid=10192` is MEASURED in `display.txt` as `A=10192`). The parcelable
//! field order comes from [`crate::parcelable`] (scanned from the framework
//! `writeToParcel`), never from memory.
//!
//! Owned methods (measured): `getApplicationInfo`, `getPackageInfo`,
//! `hasSystemFeature`, `getPackagesForUid`, `notifyDexLoad`, plus
//! `checkPermission`/`getPackageUid`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use acos_aidl::MethodInfo;

use crate::error::{ServiceError, Result};
use crate::host::{CallResult, ServiceObject};
use crate::parcelable;
use crate::value::{Parcelable, Value};

/// ACOS fixture uid (MEASURED `display.txt`: `A=10192`).
pub const FIXTURE_UID: i32 = 10192;
/// User id used in the `/data/user/<id>/` path.
pub const FIXTURE_USER: i32 = 0;

/// Typed `ApplicationInfo` values ACOS serves.
#[derive(Debug, Clone, PartialEq)]
pub struct AppInfo {
    /// Package name.
    pub package_name: String,
    /// `PackageItemInfo.name` (empty for the application).
    pub name: String,
    /// Application class name, when declared.
    pub class_name: String,
    /// `sourceDir` (the APK path).
    pub source_dir: String,
    /// `publicSourceDir`.
    pub public_source_dir: String,
    /// `dataDir`.
    pub data_dir: String,
    /// `nativeLibraryDir`.
    pub native_library_dir: String,
    /// uid.
    pub uid: i32,
    /// flags.
    pub flags: i32,
    /// private flags.
    pub private_flags: i32,
    /// targetSdkVersion.
    pub target_sdk: i32,
    /// minSdkVersion.
    pub min_sdk: i32,
    /// processName.
    pub process_name: String,
    /// theme resource id.
    pub theme: i32,
    /// label resource id.
    pub label_res: i32,
    /// `targetSandboxVersion`.
    pub target_sandbox_version: i32,
}

impl AppInfo {
    /// Build from a manifest + VFS layout.
    #[must_use]
    pub fn from_manifest(
        manifest: &acos_apk::manifest::Manifest,
        apk_path: &Path,
        uid: i32,
    ) -> Self {
        let package = manifest.package.clone();
        let source_dir = apk_path.to_string_lossy().into_owned();
        let native_library_dir = apk_path
            .parent()
            .map(|p| p.join("lib/x86_64").to_string_lossy().into_owned())
            .unwrap_or_else(|| "/data/app/lib/x86_64".into());
        let process_name = manifest
            .application
            .process
            .clone()
            .unwrap_or_else(|| package.clone());
        let class_name = manifest
            .application
            .name
            .clone()
            .unwrap_or_default();
        let theme = manifest.application.theme.unwrap_or(0) as i32;
        let label_res = manifest.application.label_ref.unwrap_or(0) as i32;
        Self {
            package_name: package.clone(),
            name: String::new(),
            class_name,
            public_source_dir: source_dir.clone(),
            source_dir,
            data_dir: format!("/data/user/{FIXTURE_USER}/{package}"),
            native_library_dir,
            uid,
            flags: 0,
            private_flags: 0,
            target_sdk: manifest.target_sdk.unwrap_or(34) as i32,
            min_sdk: manifest.min_sdk.unwrap_or(26) as i32,
            process_name,
            theme,
            label_res,
            target_sandbox_version: 2,
        }
    }

    /// The fallback fixture (used when the APK is not built).
    #[must_use]
    pub fn hello_views_fallback(apk: &Path) -> Self {
        let source = apk.to_string_lossy().into_owned();
        Self {
            package_name: "dev.acos.fixtures.helloviews".into(),
            name: String::new(),
            class_name: "dev.acos.fixtures.helloviews.MainActivity".into(),
            public_source_dir: source.clone(),
            source_dir: source,
            data_dir: "/data/user/0/dev.acos.fixtures.helloviews".into(),
            native_library_dir: "/data/app/lib/x86_64".into(),
            uid: FIXTURE_UID,
            flags: 0,
            private_flags: 0,
            target_sdk: 34,
            min_sdk: 26,
            process_name: "dev.acos.fixtures.helloviews".into(),
            theme: 0,
            label_res: 0,
            target_sandbox_version: 2,
        }
    }

    /// Encode in `ApplicationInfo.writeToParcel` order.
    #[must_use]
    pub fn to_parcelable(&self) -> Parcelable {
        Parcelable::ordered(
            "android.content.pm.ApplicationInfo",
            vec![
                Value::Int(0), // maybeWriteSquashed marker: not squashed
                Value::Str8(Some(self.name.clone())),
                Value::Str8(Some(self.package_name.clone())),
                Value::Int(0), // banner
                Value::Str(None), // label
                Value::Int(0), // icon
                Value::Int(self.label_res),
                Value::Bundle(None),
                Value::Int(0), // showUser
                Value::Int(0), // showBadge
                Value::Str8(Some(self.source_dir.clone())),
                Value::Str8(Some(self.public_source_dir.clone())),
                Value::Str8(Some(self.data_dir.clone())),
                Value::Str8(Some(self.native_library_dir.clone())),
                Value::Int(self.uid),
                Value::Int(self.flags),
                Value::Int(self.private_flags),
                Value::Int(self.target_sdk),
                Value::Int(self.min_sdk),
                Value::Int(self.theme),
                Value::Int(0),
                Value::Str8(Some(self.process_name.clone())),
                Value::Str8(Some(self.class_name.clone())),
                Value::Str8(None), // manageSpaceActivityName
                Value::Str8(None), // backupAgentName
                Value::StringArray(None),
                Value::StringArray(None),
                Value::StringArray(None),
                Value::Int(self.target_sandbox_version),
                Value::Str8(None), // permission
                Value::Str8(None), // taskAffinity
            ],
        )
    }

    /// Decode a typed parcelable produced by [`AppInfo::to_parcelable`].
    #[must_use]
    pub fn from_parcelable(p: &Parcelable) -> Option<Self> {
        let f = p.fields()?;
        Some(Self {
            name: f.get(1)?.as_str()?.to_string(),
            package_name: f.get(2)?.as_str()?.to_string(),
            label_res: f.get(6)?.as_int()?,
            source_dir: f.get(10)?.as_str()?.to_string(),
            public_source_dir: f.get(11)?.as_str()?.to_string(),
            data_dir: f.get(12)?.as_str()?.to_string(),
            native_library_dir: f.get(13)?.as_str()?.to_string(),
            uid: f.get(14)?.as_int()?,
            flags: f.get(15)?.as_int()?,
            private_flags: f.get(16)?.as_int()?,
            target_sdk: f.get(17)?.as_int()?,
            min_sdk: f.get(18)?.as_int()?,
            theme: f.get(19)?.as_int()?,
            process_name: f.get(21)?.as_str()?.to_string(),
            class_name: f.get(22)?.as_str()?.to_string(),
            target_sandbox_version: f.get(28)?.as_int()?,
        })
    }
}

/// Typed `ActivityInfo` values (the subset the fixture reads).
#[derive(Debug, Clone, PartialEq)]
pub struct ActivityInfo {
    /// Fully-qualified class name.
    pub name: String,
    /// Owning package.
    pub package_name: String,
    /// Launch mode (`standard`=0, `singleTop`=1, ...).
    pub launch_mode: i32,
    /// flags.
    pub flags: i32,
    /// theme resource id.
    pub theme: i32,
    /// `exported`.
    pub exported: bool,
    /// Parent activity (empty when none).
    pub parent_activity_name: String,
}

impl ActivityInfo {
    /// Encode in `ActivityInfo.writeToParcel` (prefix) order.
    #[must_use]
    pub fn to_parcelable(&self) -> Parcelable {
        Parcelable::ordered(
            "android.content.pm.ActivityInfo",
            vec![
                Value::Str8(Some(self.name.clone())),
                Value::Str8(Some(self.package_name.clone())),
                Value::Int(0),
                Value::Str(None),
                Value::Int(0),
                Value::Int(0),
                Value::Bundle(None),
                Value::Int(0),
                Value::Int(0),
                Value::Str8(None),
                Value::Str8(None),
                Value::StringArray(None),
                Value::Int(0),
                Value::Int(0),
                Value::Int(self.theme),
                Value::Int(self.launch_mode),
                Value::Int(self.flags),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Str8(Some(self.parent_activity_name.clone())),
                Value::Int(0),
                Value::Int(0),
            ],
        )
    }
}

/// Typed `PackageInfo` values.
#[derive(Debug, Clone, PartialEq)]
pub struct PackageInfo {
    /// Package name.
    pub package_name: String,
    /// versionCode.
    pub version_code: i32,
    /// versionName.
    pub version_name: String,
    /// The application info.
    pub application: AppInfo,
    /// Declared activities.
    pub activities: Vec<ActivityInfo>,
    /// Requested permissions.
    pub requested_permissions: Vec<String>,
    /// firstInstallTime.
    pub first_install_time: i64,
    /// lastUpdateTime.
    pub last_update_time: i64,
}

impl PackageInfo {
    /// Encode in `PackageInfo.writeToParcel` (prefix) order.
    #[must_use]
    pub fn to_parcelable(&self) -> Parcelable {
        Parcelable::ordered(
            "android.content.pm.PackageInfo",
            vec![
                Value::Str8(Some(self.package_name.clone())),
                Value::StringArray(None), // splitNames
                Value::Int(self.version_code),
                Value::Int(0), // versionCodeMajor
                Value::Str8(Some(self.version_name.clone())),
                Value::Int(0),
                Value::IntArray(None),
                Value::Str8(None),
                Value::Int(0),
                Value::Int(0),
                Value::Str8(Some(self.application.package_name.clone())),
                Value::Int(0),
                Value::Long(self.first_install_time),
                Value::Long(self.last_update_time),
                Value::IntArray(None),
                Value::StringArray(Some(
                    self.activities.iter().map(|a| Some(a.name.clone())).collect(),
                )),
                Value::StringArray(None),
                Value::StringArray(None),
                Value::StringArray(None),
                Value::Int(0),
                Value::StringArray(Some(
                    self.requested_permissions
                        .iter()
                        .map(|p| Some(p.clone()))
                        .collect(),
                )),
            ],
        )
    }
}

/// Typed feature answer.
#[derive(Debug, Clone)]
pub struct FeatureTable {
    features: Vec<(String, bool)>,
}

impl Default for FeatureTable {
    fn default() -> Self {
        // Seeded from `research/ambient-state/features.tsv` (the names the
        // framework actually asks for) and `services.tsv`'s conservative rule:
        // touch is true, webview/widgets are false, everything else false.
        let mut t = Self {
            features: Vec::new(),
        };
        for (name, enabled) in [
            ("android.hardware.touchscreen", true),
            ("android.hardware.touchscreen.multitouch", true),
            ("android.hardware.touchscreen.multitouch.distinct", true),
            ("android.hardware.faketouch", true),
            ("android.hardware.type.watch", false),
            ("android.hardware.vulkan.version", false),
            ("android.software.companion_device_setup", false),
            ("android.software.picture_in_picture", false),
            ("android.software.webview", false),
            ("android.software.app_widgets", false),
            ("android.software.input_methods", false),
        ] {
            t.features.push((name.into(), enabled));
        }
        t
    }
}

impl FeatureTable {
    /// Answer `hasSystemFeature`.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.features
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
            .unwrap_or(false)
    }

    /// Override a feature (test/scenario hook).
    pub fn set(&mut self, name: impl Into<String>, enabled: bool) {
        let name = name.into();
        if let Some(slot) = self.features.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = enabled;
        } else {
            self.features.push((name, enabled));
        }
    }

    /// All names and answers.
    #[must_use]
    pub fn entries(&self) -> &[(String, bool)] {
        &self.features
    }
}

/// The mini-PMS state.
#[derive(Debug)]
pub struct PmsState {
    /// The single installed package.
    pub app: AppInfo,
    /// The package record.
    pub package: PackageInfo,
    /// Feature answers.
    pub features: FeatureTable,
    /// Granted permissions (manifest + install `-g`).
    pub granted_permissions: Vec<String>,
    /// uid -> package.
    pub uid_packages: Vec<(i32, String)>,
    /// dex-load notifications (oneway, recorded).
    pub dex_loads: Vec<(String, String)>,
    /// installer package (null = side-loaded).
    pub installer_package: Option<String>,
    /// `sourceDir` the state was built from.
    pub apk_path: PathBuf,
}

impl PmsState {
    /// An empty state (no package installed).
    #[must_use]
    pub fn new() -> Self {
        let apk = crate::workspace_path("fixtures/build/apps/hello-views.apk")
            .unwrap_or_else(|| PathBuf::from("fixtures/build/apps/hello-views.apk"));
        let app = AppInfo::hello_views_fallback(&apk);
        let package = PackageInfo {
            package_name: app.package_name.clone(),
            version_code: 1,
            version_name: "1.0".into(),
            application: app.clone(),
            activities: vec![ActivityInfo {
                name: app.class_name.clone(),
                package_name: app.package_name.clone(),
                launch_mode: 0,
                flags: 0,
                theme: 0,
                exported: true,
                parent_activity_name: String::new(),
            }],
            requested_permissions: Vec::new(),
            first_install_time: 0,
            last_update_time: 0,
        };
        Self {
            uid_packages: vec![(app.uid, app.package_name.clone())],
            app,
            package,
            features: FeatureTable::default(),
            granted_permissions: vec!["android.permission.INTERNET".into()],
            dex_loads: Vec::new(),
            installer_package: None,
            apk_path: apk,
        }
    }

    /// Build from an APK on disk.
    pub fn from_apk(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let apk = acos_apk::Apk::open(path).map_err(|e| {
            ServiceError::Internal(format!("open {}: {e}", path.display()))
        })?;
        let manifest = apk
            .manifest()
            .map_err(|e| ServiceError::Internal(format!("manifest: {e}")))?;
        let app = AppInfo::from_manifest(&manifest, path, FIXTURE_UID);
        let activities = manifest
            .activities
            .iter()
            .map(|c| ActivityInfo {
                name: acos_apk::manifest::fq_name(&manifest.package, &c.name),
                package_name: manifest.package.clone(),
                launch_mode: launch_mode_code(c.launch_mode.as_deref()),
                flags: 0,
                theme: 0,
                exported: c.exported.unwrap_or(false),
                parent_activity_name: String::new(),
            })
            .collect();
        let requested_permissions: Vec<String> = manifest
            .uses_permissions
            .iter()
            .map(|p| p.name.clone())
            .collect();
        let version_code = manifest.version_code.unwrap_or(1) as i32;
        let version_name = manifest.version_name.clone().unwrap_or_else(|| "1.0".into());
        let package = PackageInfo {
            package_name: app.package_name.clone(),
            version_code,
            version_name,
            application: app.clone(),
            activities,
            requested_permissions: requested_permissions.clone(),
            first_install_time: 0,
            last_update_time: 0,
        };
        Ok(Self {
            uid_packages: vec![(app.uid, app.package_name.clone())],
            app,
            package,
            features: FeatureTable::default(),
            granted_permissions: requested_permissions,
            dex_loads: Vec::new(),
            installer_package: None,
            apk_path: path.to_path_buf(),
        })
    }

    /// Locate and load the fixture APK, falling back to the baked-in values.
    #[must_use]
    pub fn hello_views() -> Self {
        let path = crate::workspace_path("fixtures/build/apps/hello-views.apk")
            .unwrap_or_else(|| PathBuf::from("fixtures/build/apps/hello-views.apk"));
        Self::from_apk(&path).unwrap_or_else(|_| Self::new())
    }

    /// `PackageManager.checkPermission` policy.
    #[must_use]
    pub fn check_permission(&self, permission: &str) -> i32 {
        const GRANTED: i32 = 0;
        const DENIED: i32 = -1;
        if self.granted_permissions.iter().any(|p| p == permission) {
            GRANTED
        } else {
            DENIED
        }
    }

    /// Package name for a uid.
    #[must_use]
    pub fn packages_for_uid(&self, uid: i32) -> Vec<String> {
        self.uid_packages
            .iter()
            .filter(|(u, _)| *u == uid)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

impl Default for PmsState {
    fn default() -> Self {
        Self::new()
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

/// `IPackageManager` handler.
pub struct PmsService {
    state: Arc<Mutex<PmsState>>,
    _wiring: crate::host::Wiring,
}

impl PmsService {
    /// Wrap shared PMS state.
    #[must_use]
    pub fn new(state: Arc<Mutex<PmsState>>, wiring: crate::host::Wiring) -> Self {
        Self {
            state,
            _wiring: wiring,
        }
    }

    fn lookup<'a>(state: &'a PmsState, package: Option<&str>) -> Result<&'a PackageInfo> {
        match package {
            Some(p) if p == state.package.package_name => Ok(&state.package),
            Some("") => Ok(&state.package),
            Some(p) => Err(ServiceError::UnknownService {
                name: p.to_string(),
            }),
            None => Ok(&state.package),
        }
    }
}

impl ServiceObject for PmsService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        args: &[Value],
    ) -> Result<CallResult> {
        if interface != "android.content.pm.IPackageManager" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let mut state = self.state.lock().unwrap();
        let reply = match method.name.as_str() {
            "getApplicationInfo" => {
                let package = args.first().and_then(Value::as_str);
                let info = Self::lookup(&state, package).map(|p| p.application.clone())?;
                vec![Value::TypedObject(info.to_parcelable())]
            }
            "getPackageInfo" => {
                let package = args.first().and_then(Value::as_str);
                let info = Self::lookup(&state, package)?.clone();
                vec![Value::TypedObject(info.to_parcelable())]
            }
            "getActivityInfo" => {
                let component = args.first().and_then(Value::as_parcelable);
                let name = component
                    .and_then(|p| p.field(1))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let activity = state
                    .package
                    .activities
                    .iter()
                    .find(|a| a.name == name)
                    .cloned();
                vec![match activity {
                    Some(a) => Value::TypedObject(a.to_parcelable()),
                    None => Value::TypedObject(Parcelable::Null),
                }]
            }
            "getPackagesForUid" => {
                let uid = args.first().and_then(Value::as_int).unwrap_or(FIXTURE_UID);
                let packages = state.packages_for_uid(uid);
                vec![Value::StringArray(Some(
                    packages.into_iter().map(Some).collect(),
                ))]
            }
            "getPackageUid" => {
                let package = args.first().and_then(Value::as_str);
                match Self::lookup(&state, package) {
                    Ok(p) => vec![Value::Int(p.application.uid)],
                    Err(_) => vec![Value::Int(-1)],
                }
            }
            "hasSystemFeature" => {
                let name = args.first().and_then(Value::as_str).unwrap_or_default();
                vec![Value::Bool(state.features.has(name))]
            }
            "notifyDexLoad" => {
                let package = args.first().and_then(Value::as_str).unwrap_or_default();
                let loader = args.get(2).and_then(Value::as_str).unwrap_or_default();
                state.dex_loads.push((package.to_string(), loader.to_string()));
                Vec::new()
            }
            "checkPermission" => {
                let permission = args.first().and_then(Value::as_str).unwrap_or_default();
                vec![Value::Int(state.check_permission(permission))]
            }
            "getInstrumentationInfoAsUser" => vec![Value::TypedObject(Parcelable::Null)],
            "getInstallerPackageName" => {
                let package = args.first().and_then(Value::as_str);
                match Self::lookup(&state, package) {
                    Ok(p) if p.package_name == state.app.package_name => {
                        vec![Value::Str(state.installer_package.clone())]
                    }
                    _ => vec![Value::Str(None)],
                }
            }
            "resolveIntent" | "queryIntentActivities" | "queryIntentServices" => {
                // No intent resolution is on the hello-views launch path.
                vec![Value::TypedObject(Parcelable::Null)]
            }
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        // Keep the parcelable registry referenced so a missing codec is a test
        // failure rather than an unused import.
        let _ = parcelable::spec_for("android.content.pm.ApplicationInfo");
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}
