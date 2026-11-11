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

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use acos_aidl::MethodInfo;

use crate::error::{ServiceError, Result};
use crate::host::{CallResult, ServiceObject};
use crate::parcelable;
use crate::value::{Parcelable, Value};

/// The typed PMS description most recently produced by the service.
///
/// `acos-services` cannot see the VM, so the runtime-side materialiser needs a
/// way to obtain the *typed* values (not the prefix wire form) when it builds a
/// real `ApplicationInfo`/`PackageInfo` through the Java bridge. The dispatcher
/// is synchronous and single threaded, so the service stashes the description
/// it just served and `acos-runtime::pms_java` reads it back immediately.
#[derive(Debug, Clone, PartialEq)]
pub struct PmsDescription {
    /// The installed application.
    pub app: AppInfo,
    /// The package record (activities, version, ...).
    pub package: PackageInfo,
}

thread_local! {
    static LAST_DESCRIPTION: RefCell<Option<PmsDescription>> = const { RefCell::new(None) };
    static LAST_ACTIVITY: RefCell<Option<ActivityInfo>> = const { RefCell::new(None) };
    static LAST_PROVIDER: RefCell<Option<ProviderInfo>> = const { RefCell::new(None) };
    static LAST_PROVIDER_LIST: RefCell<Vec<ProviderInfo>> = const { RefCell::new(Vec::new()) };
}

/// Record the provider a `resolveContentProvider` call resolved (or `None`).
pub fn cache_provider_info(provider: Option<ProviderInfo>) {
    LAST_PROVIDER.with(|slot| *slot.borrow_mut() = provider);
}

/// The provider recorded by the last `resolveContentProvider` call.
#[must_use]
pub fn cached_provider_info() -> Option<ProviderInfo> {
    LAST_PROVIDER.with(|slot| slot.borrow().clone())
}

/// Record the providers a `queryContentProviders` call returned.
pub fn cache_provider_list(providers: Vec<ProviderInfo>) {
    LAST_PROVIDER_LIST.with(|slot| *slot.borrow_mut() = providers);
}

/// The providers recorded by the last `queryContentProviders` call.
#[must_use]
pub fn cached_provider_list() -> Vec<ProviderInfo> {
    LAST_PROVIDER_LIST.with(|slot| slot.borrow().clone())
}

/// Record the description a handler just served.
pub fn cache_description(desc: PmsDescription) {
    LAST_DESCRIPTION.with(|slot| *slot.borrow_mut() = Some(desc));
}

/// The description recorded by the last PMS handler call, if any.
#[must_use]
pub fn cached_description() -> Option<PmsDescription> {
    LAST_DESCRIPTION.with(|slot| slot.borrow().clone())
}

/// Record the activity a `getActivityInfo` call resolved (or `None`).
pub fn cache_activity_info(activity: Option<ActivityInfo>) {
    LAST_ACTIVITY.with(|slot| *slot.borrow_mut() = activity);
}

/// The activity recorded by the last `getActivityInfo` call.
#[must_use]
pub fn cached_activity_info() -> Option<ActivityInfo> {
    LAST_ACTIVITY.with(|slot| slot.borrow().clone())
}

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
    /// `deviceProtectedDataDir` (`/data/user_de/<user>/<pkg>`).
    pub device_protected_data_dir: String,
    /// `credentialProtectedDataDir` (`/data/user/<user>/<pkg>`).
    pub credential_protected_data_dir: String,
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
    /// `PackageItemInfo.icon` resource id.
    pub icon: i32,
    /// `ApplicationInfo.resourceDirs`.
    pub resource_dirs: Vec<String>,
    /// `ApplicationInfo.splitNames`.
    pub split_names: Vec<String>,
    /// `ApplicationInfo.sharedLibraryFiles`.
    pub shared_library_files: Vec<String>,
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
        let target_sdk = manifest.target_sdk.unwrap_or(34) as i32;
        // AOSP `ParsingPackageUtils.parseBaseApplication`:
        //   sa.getBoolean(AndroidManifestApplication_hardwareAccelerated,
        //                 owner.getTargetSdkVersion() >= ICE_CREAM_SANDWICH)
        // where ICE_CREAM_SANDWICH == 14; the result sets
        // `ApplicationInfo.FLAG_HARDWARE_ACCELERATED` (0x2000_0000). This is the
        // real device truth for targets >= 14 when the attribute is absent —
        // verified against the Android 14 `framework.jar` bytecode in
        // `results/perf/PMS_FLAGS_W151A.md` and the fixture manifest
        // (`targetSdkVersion=34`, no `android:hardwareAccelerated`).
        const ICE_CREAM_SANDWICH: i32 = 14;
        const FLAG_HARDWARE_ACCELERATED: i32 = 0x2000_0000;
        // AOSP `ParsingPackageUtils.parseBaseApplication`:
        //   sa.getBoolean(AndroidManifestApplication_hasCode, true)
        //   -> `ai.flags |= ApplicationInfo.FLAG_HAS_CODE`
        // The manifest model has no explicit `android:hasCode` field yet, so
        // the AOSP default (`true`) applies. Without it `LoadedApk.mIncludeCode`
        // is false, `getClassLoaderDexPaths()` returns an empty list and the
        // framework's `PathClassLoader` cannot load the app's own classes
        // (`ClassNotFoundException` on the manifest `android:name`) even though
        // `sourceDir` points at the staged APK.
        //
        // The value is AOSP's: `ApplicationInfo.FLAG_SYSTEM = 1 << 0` (0x1),
        // `FLAG_DEBUGGABLE = 1 << 1` (0x2), `FLAG_HAS_CODE = 1 << 2` (0x4).
        // `ActivityThread.getPackageInfo` derives `LoadedApk.mIncludeCode`
        // from `includeCode && (ai.flags & FLAG_HAS_CODE)`, which the framework
        // dex compiles as `and-int/lit8 ..., #int 4` (W174-A, disassembled from
        // `.cache/boot/android-34/dex/framework/classes.dex`). Using 0x1 here
        // set FLAG_SYSTEM and left `mIncludeCode` false, so the real
        // `PathClassLoader` was built with an empty dex path (`DexPathList[[]]`)
        // while the native-library path was populated.
        const FLAG_HAS_CODE: i32 = 0x0000_0004;
        let hardware_accelerated = manifest
            .application
            .hardware_accelerated
            .unwrap_or(target_sdk >= ICE_CREAM_SANDWICH);
        let mut flags = FLAG_HAS_CODE;
        if hardware_accelerated {
            flags |= FLAG_HARDWARE_ACCELERATED;
        }
        Self {
            package_name: package.clone(),
            name: String::new(),
            class_name,
            public_source_dir: source_dir.clone(),
            source_dir,
            data_dir: format!("/data/user/{FIXTURE_USER}/{package}"),
            device_protected_data_dir: format!("/data/user_de/{FIXTURE_USER}/{package}"),
            credential_protected_data_dir: format!("/data/user/{FIXTURE_USER}/{package}"),
            native_library_dir,
            uid,
            flags,
            private_flags: 0,
            target_sdk,
            min_sdk: manifest.min_sdk.unwrap_or(26) as i32,
            process_name,
            theme,
            label_res,
            icon: 0,
            resource_dirs: Vec::new(),
            split_names: Vec::new(),
            shared_library_files: Vec::new(),
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
            device_protected_data_dir: "/data/user_de/0/dev.acos.fixtures.helloviews".into(),
            credential_protected_data_dir: "/data/user/0/dev.acos.fixtures.helloviews".into(),
            native_library_dir: "/data/app/lib/x86_64".into(),
            uid: FIXTURE_UID,
            flags: 0,
            private_flags: 0,
            target_sdk: 34,
            min_sdk: 26,
            process_name: "dev.acos.fixtures.helloviews".into(),
            theme: 0,
            label_res: 0,
            icon: 0,
            resource_dirs: Vec::new(),
            split_names: Vec::new(),
            shared_library_files: Vec::new(),
            target_sandbox_version: 2,
        }
    }

    /// Encode in the full `ApplicationInfo.writeToParcel` order (78 fields).
    #[must_use]
    pub fn to_parcelable(&self) -> Parcelable {
        Parcelable::ordered(
            "android.content.pm.ApplicationInfo",
            vec![
                Value::Int(0), // maybeWriteSquashed marker: not squashed
                Value::Str8(Some(self.name.clone())),
                Value::Str8(Some(self.package_name.clone())),
                Value::Int(0),    // banner
                Value::Str(None), // label (TextUtils.writeToParcel)
                Value::Int(self.icon),
                Value::Int(self.label_res),
                Value::Bundle(None), // metaData
                Value::Int(0),       // showUser
                Value::Int(0),       // showBadge / isArchived
                Value::Str8(None),   // taskAffinity
                Value::Str8(None),   // permission
                Value::Str8(Some(self.process_name.clone())),
                Value::Str8(opt(self.class_name.is_empty(), self.class_name.clone())),
                Value::Int(self.theme),
                Value::Int(self.flags),
                Value::Int(self.private_flags),
                Value::Int(0), // privateFlagsExt
                Value::Int(0), // requiresSmallestWidthDp
                Value::Int(0), // compatibleWidthLimitDp
                Value::Int(0), // largestWidthLimitDp
                Value::Cond {
                    present: false,
                    values: Vec::new(),
                }, // storageUuid
                Value::Str8(None), // scanSourceDir
                Value::Str8(None), // scanPublicSourceDir
                Value::Str8(Some(self.source_dir.clone())),
                Value::Str8(Some(self.public_source_dir.clone())),
                Value::StringArray(strings(&self.split_names)),
                Value::StringArray(None), // splitSourceDirs
                Value::StringArray(None), // splitPublicSourceDirs
                Value::Skipped,           // splitDependencies (SparseArray)
                Value::Str8(Some(self.native_library_dir.clone())),
                Value::Str8(None), // secondaryNativeLibraryDir
                Value::Str8(None), // nativeLibraryRootDir
                Value::Int(0),     // nativeLibraryRootRequiresIsa
                Value::Str8(None), // primaryCpuAbi
                Value::Str8(None), // secondaryCpuAbi
                Value::StringArray(strings(&self.resource_dirs)),
                Value::StringArray(None), // overlayPaths
                Value::Str8(None),        // seInfo
                Value::Str8(None),        // seInfoUser
                Value::StringArray(strings(&self.shared_library_files)),
                Value::List(None), // sharedLibraryInfos (TypedList)
                Value::Str8(Some(self.data_dir.clone())),
                Value::Str8(Some(self.device_protected_data_dir.clone())),
                Value::Str8(Some(self.credential_protected_data_dir.clone())),
                Value::Int(self.uid),
                Value::Int(self.min_sdk),
                Value::Int(self.target_sdk),
                Value::Long(0), // longVersionCode
                Value::Int(1),  // enabled
                Value::Int(0),  // enabledSetting
                Value::Int(0),  // installLocation
                Value::Str8(None), // manageSpaceActivityName
                Value::Str8(None), // backupAgentName
                Value::Int(0),  // descriptionRes
                Value::Int(0),  // uiOptions
                Value::Int(0),  // fullBackupContent
                Value::Int(0),  // dataExtractionRulesRes
                Value::Bool(false), // crossProfile
                Value::Int(0),  // networkSecurityConfigRes
                Value::Int(0),  // category
                Value::Int(self.target_sandbox_version),
                Value::Str8(None), // classLoaderName
                Value::StringArray(None), // splitClassLoaderNames
                Value::Int(0),  // compileSdkVersion
                Value::Str8(None), // compileSdkVersionCodename
                Value::Str8(None), // appComponentFactory
                Value::Int(0),  // iconRes
                Value::Int(0),  // roundIconRes
                Value::Int(0),  // mHiddenApiPolicy
                Value::Int(0),  // hiddenUntilInstalled
                Value::Str8(None), // zygotePreloadName
                Value::Int(0),  // gwpAsanMode
                Value::Int(0),  // memtagMode
                Value::Int(0),  // nativeHeapZeroInitialized
                Value::Int(1),  // requestRawExternalStorageAccess (null -> 1)
                Value::Long(0), // createTimestamp
                Value::Cond {
                    present: false,
                    values: Vec::new(),
                }, // mAppClassNamesByProcess
                Value::Int(0), // localeConfigRes
                Value::Int(-1), // mKnownActivityEmbeddingCerts (null set -> -1)
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
            icon: f.get(5)?.as_int()?,
            label_res: f.get(6)?.as_int()?,
            process_name: f.get(12)?.as_str()?.to_string(),
            class_name: f.get(13)?.as_str().unwrap_or_default().to_string(),
            theme: f.get(14)?.as_int()?,
            flags: f.get(15)?.as_int()?,
            private_flags: f.get(16)?.as_int()?,
            source_dir: f.get(24)?.as_str()?.to_string(),
            public_source_dir: f.get(25)?.as_str()?.to_string(),
            split_names: string_array(f.get(26)),
            native_library_dir: f.get(30)?.as_str()?.to_string(),
            resource_dirs: string_array(f.get(36)),
            shared_library_files: string_array(f.get(40)),
            data_dir: f.get(42)?.as_str()?.to_string(),
            device_protected_data_dir: f.get(43)?.as_str()?.to_string(),
            credential_protected_data_dir: f.get(44)?.as_str()?.to_string(),
            uid: f.get(45)?.as_int()?,
            min_sdk: f.get(46)?.as_int()?,
            target_sdk: f.get(47)?.as_int()?,
            target_sandbox_version: f.get(61)?.as_int()?,
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
    /// Encode in the full `ActivityInfo.writeToParcel` order (43 fields).
    #[must_use]
    pub fn to_parcelable(&self, app: &AppInfo) -> Parcelable {
        Parcelable::ordered(
            "android.content.pm.ActivityInfo",
            vec![
                Value::Str8(Some(self.name.clone())),
                Value::Str8(Some(self.package_name.clone())),
                Value::Int(0),    // banner
                Value::Str(None), // label
                Value::Int(0),    // icon
                Value::Int(0),    // labelRes
                Value::Bundle(None),
                Value::Int(0), // showUser
                Value::Int(0), // showBadge
                Value::Parcelable(app.to_parcelable()), // ComponentInfo.applicationInfo
                Value::Str8(Some(self.package_name.clone())), // processName
                Value::Str8(None), // splitName
                Value::StringArray(None), // attributionTags
                Value::Int(0), // descriptionRes
                Value::Int(1), // enabled
                Value::Int(i32::from(self.exported)),
                Value::Int(0), // directBootAware
                Value::Int(self.theme),
                Value::Int(self.launch_mode),
                Value::Int(0), // documentLaunchMode
                Value::Str8(None), // permission
                Value::Str8(None), // taskAffinity
                Value::Str8(None), // targetActivity
                Value::Str8(None), // launchToken
                Value::Int(self.flags),
                Value::Int(0), // privateFlags
                Value::Int(0), // screenOrientation
                Value::Int(0), // configChanges
                Value::Int(0), // softInputMode
                Value::Int(0), // uiOptions
                Value::Str8(opt(
                    self.parent_activity_name.is_empty(),
                    self.parent_activity_name.clone(),
                )),
                Value::Int(0), // persistableMode
                Value::Int(0), // maxRecents
                Value::Int(0), // lockTaskLaunchMode
                Value::Cond {
                    present: false,
                    values: Vec::new(),
                }, // windowLayout
                Value::Int(0), // resizeMode
                Value::Str8(None), // requestedVrComponent
                Value::Int(0), // rotationAnimation
                Value::Int(0), // colorMode
                Value::Float(0.0), // mMaxAspectRatio
                Value::Float(0.0), // mMinAspectRatio
                Value::Bool(false), // supportsSizeChanges
                Value::Int(-1), // mKnownActivityEmbeddingCerts (null set -> -1)
                Value::Str8(None), // requiredDisplayCategory
            ],
        )
    }
}

/// Typed `ProviderInfo` values (the subset the framework and the fixtures read).
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderInfo {
    /// Fully-qualified provider class name.
    pub name: String,
    /// Owning package.
    pub package_name: String,
    /// `android:authorities` entry.
    pub authority: String,
    /// `processName`.
    pub process_name: String,
    /// `readPermission` (empty when the manifest declares none).
    pub read_permission: String,
    /// `writePermission`.
    pub write_permission: String,
    /// `exported`.
    pub exported: bool,
    /// `grantUriPermissions`.
    pub grant_uri_permissions: bool,
    /// `initOrder`.
    pub init_order: i32,
}

impl ProviderInfo {
    /// Encode in the full `ProviderInfo.writeToParcel` order (28 fields).
    #[must_use]
    pub fn to_parcelable(&self, app: &AppInfo) -> Parcelable {
        Parcelable::ordered(
            "android.content.pm.ProviderInfo",
            vec![
                Value::Str8(Some(self.name.clone())),
                Value::Str8(Some(self.package_name.clone())),
                Value::Int(0),    // banner
                Value::Str(None), // label
                Value::Int(0),    // icon
                Value::Int(0),    // labelRes
                Value::Bundle(None),
                Value::Int(0), // showUser
                Value::Int(0), // showBadge
                Value::Parcelable(app.to_parcelable()), // ComponentInfo.applicationInfo
                Value::Str8(Some(self.process_name.clone())),
                Value::Str8(None), // splitName
                Value::StringArray(None), // attributionTags
                Value::Int(0), // descriptionRes
                Value::Int(1), // enabled
                Value::Int(i32::from(self.exported)),
                Value::Int(0), // directBootAware
                Value::Str8(Some(self.authority.clone())),
                Value::Str8(opt(self.read_permission.is_empty(), self.read_permission.clone())),
                Value::Str8(opt(self.write_permission.is_empty(), self.write_permission.clone())),
                Value::Int(i32::from(self.grant_uri_permissions)),
                Value::Int(0), // forceUriPermissions
                Value::List(None), // uriPermissionPatterns
                Value::List(None), // pathPermissions
                Value::Int(0), // multiprocess
                Value::Int(self.init_order),
                Value::Int(0), // flags
                Value::Int(1), // isSyncable
            ],
        )
    }

    /// Decode a typed parcelable produced by [`ProviderInfo::to_parcelable`].
    #[must_use]
    pub fn from_parcelable(p: &Parcelable) -> Option<Self> {
        let f = p.fields()?;
        Some(Self {
            name: f.first()?.as_str()?.to_string(),
            package_name: f.get(1)?.as_str()?.to_string(),
            process_name: f.get(10)?.as_str()?.to_string(),
            authority: f.get(17)?.as_str()?.to_string(),
            read_permission: f.get(18)?.as_str().unwrap_or_default().to_string(),
            write_permission: f.get(19)?.as_str().unwrap_or_default().to_string(),
            exported: f.get(15)?.as_int()? != 0,
            grant_uri_permissions: f.get(20)?.as_int()? != 0,
            init_order: f.get(25)?.as_int()?,
        })
    }
}

/// Typed `ServiceInfo` values (the subset the framework and the fixtures read).
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceInfo {
    /// Fully-qualified service class name.
    pub name: String,
    /// Owning package.
    pub package_name: String,
    /// `processName`.
    pub process_name: String,
    /// `permission` (empty when the manifest declares none).
    pub permission: String,
    /// `exported`.
    pub exported: bool,
    /// `flags`.
    pub flags: i32,
    /// `mForegroundServiceType`.
    pub foreground_service_type: i32,
}

impl ServiceInfo {
    /// Encode in the full `ServiceInfo.writeToParcel` order (20 fields).
    #[must_use]
    pub fn to_parcelable(&self, app: &AppInfo) -> Parcelable {
        Parcelable::ordered(
            "android.content.pm.ServiceInfo",
            vec![
                // ComponentInfo prefix
                Value::Str8(Some(self.name.clone())),
                Value::Str8(Some(self.package_name.clone())),
                Value::Int(0),    // banner
                Value::Str(None), // label
                Value::Int(0),    // icon
                Value::Int(0),    // labelRes
                Value::Bundle(None),
                Value::Int(0), // showUser
                Value::Int(0), // showBadge
                Value::Parcelable(app.to_parcelable()),
                Value::Str8(Some(self.process_name.clone())),
                Value::Str8(None),        // splitName
                Value::StringArray(None), // attributionTags
                Value::Int(0),            // descriptionRes
                Value::Int(1),            // enabled
                Value::Int(i32::from(self.exported)),
                Value::Int(0), // directBootAware
                // ServiceInfo
                Value::Str8(opt(self.permission.is_empty(), self.permission.clone())),
                Value::Int(self.flags),
                Value::Int(self.foreground_service_type),
            ],
        )
    }

    /// Decode a typed parcelable produced by [`ServiceInfo::to_parcelable`].
    #[must_use]
    pub fn from_parcelable(p: &Parcelable) -> Option<Self> {
        let f = p.fields()?;
        Some(Self {
            name: f.first()?.as_str()?.to_string(),
            package_name: f.get(1)?.as_str()?.to_string(),
            process_name: f.get(10)?.as_str()?.to_string(),
            permission: f.get(17)?.as_str().unwrap_or_default().to_string(),
            exported: f.get(15)?.as_int()? != 0,
            flags: f.get(18)?.as_int()?,
            foreground_service_type: f.get(19)?.as_int()?,
        })
    }
}

fn opt(empty: bool, value: String) -> Option<String> {
    if empty {
        None
    } else {
        Some(value)
    }
}

fn strings(values: &[String]) -> Option<Vec<Option<String>>> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().cloned().map(Some).collect())
    }
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::StringArray(Some(items))) => items.iter().filter_map(Clone::clone).collect(),
        _ => Vec::new(),
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
    /// Encode in the full `PackageInfo.writeToParcel` order (41 fields).
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
                Value::Int(0),         // baseRevisionCode
                Value::IntArray(None), // splitRevisionCodes
                Value::Str8(None),     // sharedUserId
                Value::Int(0),         // sharedUserLabel
                Value::Cond {
                    present: true,
                    values: vec![Value::Parcelable(self.application.to_parcelable())],
                }, // applicationInfo
                Value::Long(self.first_install_time),
                Value::Long(self.last_update_time),
                Value::IntArray(None), // gids
                Value::List(None),     // activities
                Value::List(None),     // receivers
                Value::List(None),     // services
                Value::List(None),     // providers
                Value::List(None),     // instrumentation
                Value::List(None),     // permissions
                Value::StringArray(Some(
                    self.requested_permissions
                        .iter()
                        .cloned()
                        .map(Some)
                        .collect(),
                )),
                Value::IntArray(None), // requestedPermissionsFlags
                Value::List(None),     // signatures
                Value::List(None),     // configPreferences
                Value::List(None),     // reqFeatures
                Value::List(None),     // featureGroups
                Value::List(None),     // attributions
                Value::Int(0),         // installLocation
                Value::Int(0),         // isStub
                Value::Int(0),         // coreApp
                Value::Int(0),         // requiredForAllUsers
                Value::Str8(None),     // restrictedAccountType
                Value::Str8(None),     // requiredAccountType
                Value::Str8(None),     // overlayTarget
                Value::Str8(None),     // overlayCategory
                Value::Int(0),         // overlayPriority
                Value::Bool(false),    // mOverlayIsStatic
                Value::Int(0),         // compileSdkVersion
                Value::Str8(None),     // compileSdkVersionCodename
                Value::Cond {
                    present: false,
                    values: Vec::new(),
                }, // signingInfo
                Value::Bool(false), // isApex
                Value::Bool(false), // isActiveApex
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
    /// Declared content providers (`<provider>` + `android:authorities`).
    pub providers: Vec<ProviderInfo>,
    /// Declared services (`<service>`).
    pub services: Vec<ServiceInfo>,
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
            providers: Vec::new(),
            services: Vec::new(),
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
                flags: 0x200, // ActivityInfo.FLAG_HARDWARE_ACCELERATED (API default, targetSdk >= 14)
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
        let provider_package = manifest.package.clone();
        let mut providers: Vec<ProviderInfo> = Vec::new();
        for c in &manifest.providers {
            let name = acos_apk::manifest::fq_name(&provider_package, &c.name);
            let process = c
                .process
                .clone()
                .unwrap_or_else(|| provider_package.clone());
            let permission = c.permission.clone().unwrap_or_default();
            let exported = c.exported.unwrap_or(false);
            for authority in &c.authorities {
                providers.push(ProviderInfo {
                    name: name.clone(),
                    package_name: provider_package.clone(),
                    authority: authority.clone(),
                    process_name: process.clone(),
                    read_permission: permission.clone(),
                    write_permission: permission.clone(),
                    exported,
                    grant_uri_permissions: false,
                    init_order: 0,
                });
            }
        }
        let services: Vec<ServiceInfo> = manifest
            .services
            .iter()
            .map(|c| {
                let name = acos_apk::manifest::fq_name(&manifest.package, &c.name);
                let process = c
                    .process
                    .clone()
                    .unwrap_or_else(|| manifest.package.clone());
                ServiceInfo {
                    name,
                    package_name: manifest.package.clone(),
                    process_name: process,
                    permission: c.permission.clone().unwrap_or_default(),
                    exported: c.exported.unwrap_or(false),
                    flags: 0,
                    foreground_service_type: 0,
                }
            })
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
            providers,
            services,
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

    /// The declared provider matching `authority`, if any.
    #[must_use]
    pub fn provider_for_authority(&self, authority: &str) -> Option<ProviderInfo> {
        self.providers
            .iter()
            .find(|p| p.authority == authority)
            .cloned()
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

    /// Replace the installed package with the APK being launched.
    ///
    /// `ServiceHost::standard()` seeds the PMS with the `hello-views` fixture;
    /// the launch path re-points it at the real APK before the framework asks
    /// for `getApplicationInfo`. A missing APK is a structured error, never a
    /// silent fallback to the previous package.
    pub fn set_package_from_apk(&mut self, apk_path: &Path) -> Result<()> {
        let state = PmsState::from_apk(apk_path)?;
        *self.state.lock().unwrap() = state;
        Ok(())
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
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

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
        cache_description(PmsDescription {
            app: state.app.clone(),
            package: state.package.clone(),
        });
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
                // `ComponentName.writeToParcel` writes `mPackage` then `mClass`.
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
                cache_activity_info(activity.clone());
                vec![match activity {
                    Some(a) => Value::TypedObject(a.to_parcelable(&state.app)),
                    None => Value::TypedObject(Parcelable::Null),
                }]
            }
            "getServiceInfo" => {
                // `getServiceInfo(ComponentName, long flags, int userId)`.
                let component = args.first().and_then(Value::as_parcelable);
                let name = component
                    .and_then(|p| p.field(1))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let service = state.services.iter().find(|s| s.name == name).cloned();
                vec![match service {
                    Some(s) => Value::TypedObject(s.to_parcelable(&state.app)),
                    None => Value::TypedObject(Parcelable::Null),
                }]
            }
            "getProviderInfo" => {
                // `getProviderInfo(ComponentName, long flags, int userId)`.
                let component = args.first().and_then(Value::as_parcelable);
                let name = component
                    .and_then(|p| p.field(1))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let provider = state.providers.iter().find(|p| p.name == name).cloned();
                vec![match provider {
                    Some(p) => Value::TypedObject(p.to_parcelable(&state.app)),
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
            // `acos-apk` does not parse `<instrumentation>` yet, so there is no
            // manifest-backed `InstrumentationInfo` to serve. A structured
            // `UNSUPPORTED_ANDROID_API` is honest rather than a fabricated null.
            "getInstrumentationInfo" | "getInstrumentationInfoAsUser" => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
            "getInstallerPackageName" => {
                let package = args.first().and_then(Value::as_str);
                match Self::lookup(&state, package) {
                    Ok(p) if p.package_name == state.app.package_name => {
                        vec![Value::Str(state.installer_package.clone())]
                    }
                    _ => vec![Value::Str(None)],
                }
            }
            "resolveContentProvider" => {
                // `resolveContentProvider(String authority, long flags, int userId)`.
                let authority = args.first().and_then(Value::as_str).unwrap_or_default();
                let provider = state.provider_for_authority(authority);
                cache_provider_info(provider.clone());
                vec![match provider {
                    Some(p) => Value::TypedObject(p.to_parcelable(&state.app)),
                    None => Value::TypedObject(Parcelable::Null),
                }]
            }
            "queryContentProviders" => {
                // `queryContentProviders(String processName, int uid, long flags,
                // String metadataKey)`. The reply is a `ParceledListSlice`; the
                // real list is materialised by `pms_java` from the cache.
                cache_provider_list(state.providers.clone());
                vec![Value::TypedObject(Parcelable::Null)]
            }
            "resolveIntent" | "queryIntentActivities" => {
                // The launcher `ResolveInfo` is materialised by `pms_java` from
                // the cached description, so the generic reply is a placeholder.
                vec![Value::TypedObject(Parcelable::Null)]
            }
            // A service `ResolveInfo` uses the component marker `2` and a
            // `ParceledListSlice` for `queryIntentServices`; neither is modelled
            // by the activity-branch `ResolveInfo` spec yet. Structured
            // `UNSUPPORTED` instead of an empty/false match.
            "resolveService" | "queryIntentServices" => {
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
        // Keep the parcelable registry referenced so a missing codec is a test
        // failure rather than an unused import.
        let _ = parcelable::spec_for("android.content.pm.ApplicationInfo");
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AOSP `parseBaseApplication` sets `FLAG_HAS_CODE` from
    /// `android:hasCode` (default `true`). Without it `LoadedApk.mIncludeCode`
    /// is false and the framework's `PathClassLoader` gets an empty dex path,
    /// so an app with a manifest `android:name` cannot be instantiated
    /// (W164-A: `ClassNotFoundException: ... MainApplication`,
    /// `DexPathList[[]]`).
    #[test]
    fn application_info_flags_include_has_code() {
        let manifest = acos_apk::manifest::Manifest {
            package: "dev.acos.fixtures.rnhello".into(),
            version_code: Some(1),
            version_code_major: None,
            version_name: Some("1.0".into()),
            min_sdk: Some(26),
            target_sdk: Some(34),
            max_sdk: None,
            compile_sdk: Some(34),
            uses_permissions: Vec::new(),
            uses_features: Vec::new(),
            application: acos_apk::manifest::ApplicationInfo {
                name: Some(".MainApplication".into()),
                label: None,
                label_ref: None,
                debuggable: Some(true),
                allow_backup: Some(false),
                extract_native_libs: None,
                hardware_accelerated: None,
                theme: None,
                process: None,
            },
            activities: Vec::new(),
            services: Vec::new(),
            receivers: Vec::new(),
            providers: Vec::new(),
        };
        let app = AppInfo::from_manifest(&manifest, Path::new("/tmp/app.apk"), FIXTURE_UID);
        const FLAG_HAS_CODE: i32 = 0x0000_0004;
        const FLAG_HARDWARE_ACCELERATED: i32 = 0x2000_0000;
        assert_eq!(
            app.flags & FLAG_HAS_CODE,
            FLAG_HAS_CODE,
            "FLAG_HAS_CODE must be set for the default hasCode=true"
        );
        assert_eq!(
            app.flags & FLAG_HARDWARE_ACCELERATED,
            FLAG_HARDWARE_ACCELERATED,
            "targetSdk 34 defaults hardwareAccelerated to true"
        );
    }

    fn sample_provider() -> ProviderInfo {
        ProviderInfo {
            name: "dev.acos.fixtures.svcprovider.EchoProvider".into(),
            package_name: "dev.acos.fixtures.svcprovider".into(),
            authority: "dev.acos.fixtures.svcprovider".into(),
            process_name: "dev.acos.fixtures.svcprovider".into(),
            read_permission: String::new(),
            write_permission: String::new(),
            exported: true,
            grant_uri_permissions: false,
            init_order: 0,
        }
    }

    fn sample_service() -> ServiceInfo {
        ServiceInfo {
            name: "dev.acos.fixtures.svcprovider.EchoService".into(),
            package_name: "dev.acos.fixtures.svcprovider".into(),
            process_name: "dev.acos.fixtures.svcprovider".into(),
            permission: "dev.acos.fixtures.ECHO".into(),
            exported: false,
            flags: 0,
            foreground_service_type: 0,
        }
    }

    #[test]
    fn provider_round_trips_through_the_prefix_codec() {
        let provider = sample_provider();
        let app = AppInfo::hello_views_fallback(Path::new("hello-views.apk"));
        let decoded =
            ProviderInfo::from_parcelable(&provider.to_parcelable(&app)).expect("decoded");
        assert_eq!(decoded, provider);
    }

    #[test]
    fn service_round_trips_through_the_full_codec() {
        let service = sample_service();
        let app = AppInfo::hello_views_fallback(Path::new("hello-views.apk"));
        let decoded =
            ServiceInfo::from_parcelable(&service.to_parcelable(&app)).expect("decoded");
        assert_eq!(decoded, service);
    }

    #[test]
    fn resolve_content_provider_answers_the_declared_authority() {
        let mut state = PmsState::new();
        assert!(state.provider_for_authority("dev.acos.fixtures.svcprovider").is_none());
        state.providers.push(sample_provider());
        let found = state
            .provider_for_authority("dev.acos.fixtures.svcprovider")
            .expect("declared authority resolves");
        assert_eq!(found.name, sample_provider().name);
        assert!(state.provider_for_authority("dev.acos.fixtures.absent").is_none());
    }

    #[test]
    fn from_apk_reads_provider_authorities() {
        let path = crate::workspace_path("fixtures/build/apps/service-provider.apk")
            .unwrap_or_else(|| PathBuf::from("fixtures/build/apps/service-provider.apk"));
        if !path.is_file() {
            eprintln!("SKIP from_apk_reads_provider_authorities: fixture APK not built");
            return;
        }
        let state = PmsState::from_apk(&path).expect("from_apk");
        let provider = state
            .provider_for_authority("dev.acos.fixtures.svcprovider")
            .expect("fixture authority declared in the manifest");
        assert_eq!(provider.name, "dev.acos.fixtures.svcprovider.EchoProvider");
        assert!(!provider.exported, "the fixture provider is not exported");
        let service = state
            .services
            .iter()
            .find(|s| s.name == "dev.acos.fixtures.svcprovider.EchoService")
            .expect("fixture service declared in the manifest");
        assert!(!service.exported, "the fixture service is not exported");
    }
}
