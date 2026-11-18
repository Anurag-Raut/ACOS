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
use acos_apk::manifest::DeclaredPermission;
use acos_parcel::Parcel;

/// The parsed `<meta-data>` model, re-exported so a crate that only depends on
/// `acos-services` (the ART launcher building the guest's `ProviderInfo`s) can
/// name the entries without a second manifest parser.
pub use acos_apk::manifest::{MetaDataEntry, MetaDataValue};

use crate::error::{Result, ServiceError};
use crate::host::{CallResult, ServiceObject};
use crate::parcelable;
use crate::value::{Bundle, Parcelable, Value};

/// `PackageManager.GET_META_DATA` (API 34 `PackageManager.java`): the caller is
/// asking for the `PackageItemInfo.metaData` `Bundle`.
///
/// `PackageParser` keeps the parsed `<meta-data>` off the installed
/// `ApplicationInfo` (`Package.mAppMetaData`) and off the installed component
/// infos (`Component.metaData`); `generateApplicationInfo`/
/// `generateActivityInfo`/`generateServiceInfo`/`generateProviderInfo` copy it
/// into the reply only when this flag is set (`PackageParser.copyNeeded`).
pub const GET_META_DATA: i64 = 0x0000_0080;

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
    static LAST_ACTIVITY: RefCell<Option<(AppInfo, ActivityInfo)>> = const { RefCell::new(None) };
    static LAST_PROVIDER: RefCell<Option<(AppInfo, ProviderInfo)>> = const { RefCell::new(None) };
    static LAST_PROVIDER_LIST: RefCell<Vec<(AppInfo, ProviderInfo)>> = const { RefCell::new(Vec::new()) };
    static LAST_INTENT_ACTIVITIES: RefCell<Vec<ResolvedActivity>> = const { RefCell::new(Vec::new()) };
}

/// One activity a `queryIntentActivities`/`resolveIntent` call matched, with
/// the package it belongs to.
///
/// A session installs more than one package, so the materialiser cannot use
/// "the" installed application: each match carries its own `AppInfo` (uid,
/// `sourceDir`) and the declared filters the `ResolveInfo.filter` is built
/// from.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedActivity {
    /// The application record of the package that declares the activity.
    pub app: AppInfo,
    /// The activity record.
    pub activity: ActivityInfo,
    /// The declared component (`ams::DeclaredComponent`): the intent filters.
    pub declared: crate::ams::DeclaredComponent,
}

/// Record the provider a `resolveContentProvider` call resolved (or `None`).
pub fn cache_provider_info(provider: Option<(AppInfo, ProviderInfo)>) {
    LAST_PROVIDER.with(|slot| *slot.borrow_mut() = provider);
}

/// The provider recorded by the last `resolveContentProvider` call, with its
/// owning package's application record.
#[must_use]
pub fn cached_provider_info() -> Option<(AppInfo, ProviderInfo)> {
    LAST_PROVIDER.with(|slot| slot.borrow().clone())
}

/// Record the providers a `queryContentProviders` call returned.
pub fn cache_provider_list(providers: Vec<(AppInfo, ProviderInfo)>) {
    LAST_PROVIDER_LIST.with(|slot| *slot.borrow_mut() = providers);
}

/// The providers recorded by the last `queryContentProviders` call.
#[must_use]
pub fn cached_provider_list() -> Vec<(AppInfo, ProviderInfo)> {
    LAST_PROVIDER_LIST.with(|slot| slot.borrow().clone())
}

/// Record the activities the last `queryIntentActivities`/`resolveIntent`
/// call matched.
pub fn cache_intent_activities(activities: Vec<ResolvedActivity>) {
    LAST_INTENT_ACTIVITIES.with(|slot| *slot.borrow_mut() = activities);
}

/// The activities recorded by the last intent query.
#[must_use]
pub fn cached_intent_activities() -> Vec<ResolvedActivity> {
    LAST_INTENT_ACTIVITIES.with(|slot| slot.borrow().clone())
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

/// Record the activity a `getActivityInfo` call resolved (or `None`), with the
/// application record of the package that declares it.
pub fn cache_activity_info(activity: Option<(AppInfo, ActivityInfo)>) {
    LAST_ACTIVITY.with(|slot| *slot.borrow_mut() = activity);
}

/// The activity recorded by the last `getActivityInfo` call.
#[must_use]
pub fn cached_activity_info() -> Option<(AppInfo, ActivityInfo)> {
    LAST_ACTIVITY.with(|slot| slot.borrow().clone())
}

/// ACOS fixture uid (MEASURED `display.txt`: `A=10192`).
///
/// It is the uid the package manager allocates to the **first** package of a
/// session (AOSP allocates 10000+ per installed package); a second package gets
/// the next free uid, so a multi-process session has one uid per app.
pub const FIXTURE_UID: i32 = 10192;
/// `Process.FIRST_APPLICATION_UID`: the first uid the package manager allocates.
pub const FIRST_APPLICATION_UID: u32 = 10_000;
/// `Process.LAST_APPLICATION_UID`: the last application uid (`Process`'s range).
pub const LAST_APPLICATION_UID: u32 = 19_999;
/// User id used in the `/data/user/<id>/` path.
pub const FIXTURE_USER: i32 = 0;

/// Whether `uid` is an **application** uid, i.e. a uid the package manager
/// allocates for an installed package (`Process.isApplicationUid`).
///
/// The guest must carry one of these: uid 1000 is `SYSTEM_UID` (the host caller,
/// and the uid the framework takes privileged branches on).
#[must_use]
pub fn is_application_uid(uid: u32) -> bool {
    (FIRST_APPLICATION_UID..=LAST_APPLICATION_UID).contains(&uid)
}

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
    /// `<application>` `<meta-data>`, exactly as `PackageParser.parseMetaData`
    /// parsed it (`Package.mAppMetaData`). Sent in the `metaData` Bundle only
    /// when the caller passes [`GET_META_DATA`].
    pub meta_data: Vec<MetaDataEntry>,
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
        // `nativeLibraryDir` must be a *guest* path: Android framework code
        // (`SoLoader`'s `ApplicationSoSource`/`DirectorySoSource`) resolves and
        // canonicalizes it through `java.io.File`, i.e. through the sandbox
        // VFS. `NativeBridge::prepare` extracts the APK's `lib/<abi>/*.so` into
        // exactly `<dataDir>/lib/<abi>` inside that VFS, so point there. (A
        // host build path can never be canonicalized by the confined guest and
        // made the first `SoLoader.loadLibrary` fail with an `ENOENT`.)
        let native_library_dir = format!(
            "/data/user/{FIXTURE_USER}/{package}/lib/{}",
            acos_apk::HOST_ABI
        );
        let process_name = manifest
            .application
            .process
            .clone()
            .unwrap_or_else(|| package.clone());
        let class_name = manifest.application.name.clone().unwrap_or_default();
        let theme = manifest.application.theme.unwrap_or(0) as i32;
        let label_res = manifest.application.label_ref.unwrap_or(0) as i32;
        let target_sdk = manifest.target_sdk.unwrap_or(34) as i32;
        // W36 (W31 R1): the flag derivation lives in `acos-apk`, transcribed
        // from the image's `PackageParser.parseBaseApplication`
        // (android-14.0.0_r75): hasCode/extractNativeLibs/allowBackup/
        // supportsRtl/usesCleartextTraffic (targetSdk default)/largeHeap/
        // debuggable/vmSafeMode/isGame/testOnly, plus hardwareAccelerated
        // (targetSdk >= 14). Before this the default path received four
        // hand-computed bits (`0x10800004`: no debuggable, no backup, no RTL,
        // and — the measured wall — no cleartext), so a manifest-correct app
        // behaved differently from its own declaration.
        let flags = manifest.application_flags();
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
            meta_data: manifest.application.meta_data.clone(),
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
            meta_data: Vec::new(),
        }
    }

    /// Encode in the full `ApplicationInfo.writeToParcel` order (78 fields).
    ///
    /// `metaData` is present only when `flags` contains [`GET_META_DATA`], which
    /// is what `PackageParser.generateApplicationInfo` does.
    #[must_use]
    pub fn to_parcelable_with_flags(&self, flags: i64) -> Parcelable {
        Parcelable::ordered(
            "android.content.pm.ApplicationInfo",
            vec![
                Value::Int(0), // maybeWriteSquashed marker: not squashed
                // `PackageItemInfo.writeToParcel` (the image's shape):
                //   name, packageName, **labelRes**, label, icon, **logo**,
                //   metaData, banner, showUser.
                // The label resource comes *before* the inline label, and the
                // logo slot follows the icon; writing them in the other order
                // put `labelRes` into `logo` on the guest (MPROC7: proc-b's
                // window asked Drawable(R.string.app_name) because
                // `ComponentInfo.getLogoResource()` read `applicationInfo.logo`).
                Value::Str8(Some(self.name.clone())),
                Value::Str8(Some(self.package_name.clone())),
                Value::Int(self.label_res),              // labelRes
                Value::Str(None),                        // label (TextUtils.writeToParcel)
                Value::Int(self.icon),                   // icon
                Value::Int(0),                           // logo
                meta_data_value(&self.meta_data, flags), // metaData
                Value::Int(0),                           // banner
                Value::Int(0),                           // showUser
                Value::Str8(None),                       // taskAffinity
                Value::Str8(None),                       // permission
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
                Value::Long(0),     // longVersionCode
                Value::Int(1),      // enabled
                Value::Int(0),      // enabledSetting
                Value::Int(0),      // installLocation
                Value::Str8(None),  // manageSpaceActivityName
                Value::Str8(None),  // backupAgentName
                Value::Int(0),      // descriptionRes
                Value::Int(0),      // uiOptions
                Value::Int(0),      // fullBackupContent
                Value::Int(0),      // dataExtractionRulesRes
                Value::Bool(false), // crossProfile
                Value::Int(0),      // networkSecurityConfigRes
                Value::Int(0),      // category
                Value::Int(self.target_sandbox_version),
                Value::Str8(None),        // classLoaderName
                Value::StringArray(None), // splitClassLoaderNames
                Value::Int(0),            // compileSdkVersion
                Value::Str8(None),        // compileSdkVersionCodename
                Value::Str8(None),        // appComponentFactory
                Value::Int(0),            // iconRes
                Value::Int(0),            // roundIconRes
                Value::Int(0),            // mHiddenApiPolicy
                Value::Int(0),            // hiddenUntilInstalled
                Value::Str8(None),        // zygotePreloadName
                Value::Int(0),            // gwpAsanMode
                Value::Int(0),            // memtagMode
                Value::Int(0),            // nativeHeapZeroInitialized
                Value::Int(1),            // requestRawExternalStorageAccess (null -> 1)
                Value::Long(0),           // createTimestamp
                Value::Cond {
                    present: false,
                    values: Vec::new(),
                }, // mAppClassNamesByProcess
                Value::Int(0),            // localeConfigRes
                Value::Int(-1),           // mKnownActivityEmbeddingCerts (null set -> -1)
            ],
        )
    }

    /// Encode without the metadata `Bundle` (the flags-off form).
    #[must_use]
    pub fn to_parcelable(&self) -> Parcelable {
        self.to_parcelable_with_flags(0)
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
            meta_data: Vec::new(),
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
    /// `<activity>` `<meta-data>` (`Component.metaData`); see [`GET_META_DATA`].
    pub meta_data: Vec<MetaDataEntry>,
}

impl ActivityInfo {
    /// Encode in the full `ActivityInfo.writeToParcel` order (43 fields).
    /// `metaData` follows the request flags, like
    /// `PackageParser.generateActivityInfo`.
    #[must_use]
    pub fn to_parcelable_with_flags(&self, app: &AppInfo, flags: i64) -> Parcelable {
        Parcelable::ordered(
            "android.content.pm.ActivityInfo",
            vec![
                Value::Str8(Some(self.name.clone())),
                Value::Str8(Some(self.package_name.clone())),
                Value::Int(0),    // labelRes (the activity declares no label)
                Value::Str(None), // label
                Value::Int(0),    // icon
                Value::Int(0),    // logo
                meta_data_value(&self.meta_data, flags),
                Value::Int(0),                                          // banner
                Value::Int(0),                                          // showUser
                Value::Parcelable(app.to_parcelable_with_flags(flags)), // ComponentInfo.applicationInfo
                Value::Str8(Some(self.package_name.clone())),           // processName
                Value::Str8(None),                                      // splitName
                Value::StringArray(None),                               // attributionTags
                Value::Int(0),                                          // descriptionRes
                Value::Int(1),                                          // enabled
                Value::Int(i32::from(self.exported)),
                Value::Int(0), // directBootAware
                Value::Int(self.theme),
                Value::Int(self.launch_mode),
                Value::Int(0),     // documentLaunchMode
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

    /// Encode without the metadata `Bundle` (the flags-off form).
    #[must_use]
    pub fn to_parcelable(&self, app: &AppInfo) -> Parcelable {
        self.to_parcelable_with_flags(app, 0)
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
    /// `<provider>` `<meta-data>` (`Component.metaData`); see [`GET_META_DATA`].
    pub meta_data: Vec<MetaDataEntry>,
}

impl ProviderInfo {
    /// Encode in the full `ProviderInfo.writeToParcel` order (28 fields).
    ///
    /// `metaData` follows the request flags, like
    /// `PackageParser.generateProviderInfo`: `FileProvider` re-resolves its own
    /// authority with `GET_META_DATA` and reads
    /// `android.support.FILE_PROVIDER_PATHS` from this field, and
    /// `androidx.startup`'s `InitializationProvider` reads its initializer list
    /// from it the same way.
    #[must_use]
    pub fn to_parcelable_with_flags(&self, app: &AppInfo, flags: i64) -> Parcelable {
        Parcelable::ordered(
            "android.content.pm.ProviderInfo",
            vec![
                Value::Str8(Some(self.name.clone())),
                Value::Str8(Some(self.package_name.clone())),
                Value::Int(0),    // banner
                Value::Str(None), // label
                Value::Int(0),    // icon
                Value::Int(0),    // labelRes
                meta_data_value(&self.meta_data, flags),
                Value::Int(0),                                          // showUser
                Value::Int(0),                                          // showBadge
                Value::Parcelable(app.to_parcelable_with_flags(flags)), // ComponentInfo.applicationInfo
                Value::Str8(Some(self.process_name.clone())),
                Value::Str8(None),        // splitName
                Value::StringArray(None), // attributionTags
                Value::Int(0),            // descriptionRes
                Value::Int(1),            // enabled
                Value::Int(i32::from(self.exported)),
                Value::Int(0), // directBootAware
                Value::Str8(Some(self.authority.clone())),
                Value::Str8(opt(
                    self.read_permission.is_empty(),
                    self.read_permission.clone(),
                )),
                Value::Str8(opt(
                    self.write_permission.is_empty(),
                    self.write_permission.clone(),
                )),
                Value::Int(i32::from(self.grant_uri_permissions)),
                Value::Int(0),     // forceUriPermissions
                Value::List(None), // uriPermissionPatterns
                Value::List(None), // pathPermissions
                Value::Int(0),     // multiprocess
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
            meta_data: Vec::new(),
        })
    }

    /// Encode without the metadata `Bundle` (the flags-off form).
    #[must_use]
    pub fn to_parcelable(&self, app: &AppInfo) -> Parcelable {
        self.to_parcelable_with_flags(app, 0)
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
    /// `<service>` `<meta-data>` (`Component.metaData`); see [`GET_META_DATA`].
    pub meta_data: Vec<MetaDataEntry>,
}

impl ServiceInfo {
    /// Encode in the full `ServiceInfo.writeToParcel` order (20 fields).
    ///
    /// `metaData` follows the request flags, like
    /// `PackageParser.generateServiceInfo`.
    #[must_use]
    pub fn to_parcelable_with_flags(&self, app: &AppInfo, flags: i64) -> Parcelable {
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
                meta_data_value(&self.meta_data, flags),
                Value::Int(0), // showUser
                Value::Int(0), // showBadge
                Value::Parcelable(app.to_parcelable_with_flags(flags)),
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
            meta_data: Vec::new(),
        })
    }

    /// Encode without the metadata `Bundle` (the flags-off form).
    #[must_use]
    pub fn to_parcelable(&self, app: &AppInfo) -> Parcelable {
        self.to_parcelable_with_flags(app, 0)
    }
}

fn opt(empty: bool, value: String) -> Option<String> {
    if empty {
        None
    } else {
        Some(value)
    }
}

/// The `metaData` field of a `PackageItemInfo` reply.
///
/// `PackageParser.generate*Info` hands the parsed `Bundle` to the caller only
/// when the request carries [`GET_META_DATA`]; without the flag the parcelled
/// field is `null`. A component that declares no `<meta-data>` also replies
/// `null` even with the flag (there is nothing to copy).
fn meta_data_value(entries: &[MetaDataEntry], flags: i64) -> Value {
    if flags & GET_META_DATA == 0 || entries.is_empty() {
        return Value::Bundle(None);
    }
    let mut bundle = Bundle::new();
    for entry in entries {
        // The `Bundle` value types `parseMetaData` writes: `putString`,
        // `putInt` (also for an `android:resource` id), `putBoolean`, `putFloat`.
        let value = match &entry.value {
            MetaDataValue::String(text) => Value::Str(Some(text.clone())),
            MetaDataValue::Int(number) => Value::Int(*number),
            MetaDataValue::Boolean(flag) => Value::Bool(*flag),
            MetaDataValue::Float(number) => Value::Float(*number),
            MetaDataValue::Reference(id) => Value::Int(*id as i32),
        };
        bundle.put(entry.name.clone(), value);
    }
    Value::Bundle(Some(bundle))
}

/// K4 diagnostic: `ACOS_PMS_TRACE=<path>` appends one line per
/// metadata-bearing query (method, key, the flags actually received, and how
/// many `<meta-data>` entries the reply carries). It is how the `GET_META_DATA`
/// gate is observed on the wire rather than assumed; unset (the default) it is
/// inert.
fn trace_query(method: &str, key: &str, flags: i64, entries: usize) {
    let Some(path) = std::env::var_os("ACOS_PMS_TRACE") else {
        return;
    };
    use std::io::Write as _;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(
            file,
            "{method} key={key} flags={flags:#x} get_meta_data={} meta_entries={entries}",
            flags & GET_META_DATA != 0
        );
    }
}

/// WV2: `ACOS_PARCEL_DUMP=<path>` records the wire bytes of one typed reply
/// (the sibling of `ACOS_PMS_TRACE`, which records which query was answered).
/// It encodes the value with the Rust spec — the bytes a Rust-written reply
/// carries — so the provider/`webviewupdate` replies can be byte-diffed against
/// the AOSP `writeToParcel` order. Inert unless the hook is set.
fn dump_reply(tag: &str, value: &Parcelable) {
    if !crate::parcel_dump::enabled() {
        return;
    }
    let mut parcel = Parcel::new();
    if crate::parcelable::write_payload(&mut parcel, value).is_ok() {
        crate::parcel_dump::dump(tag, &parcel);
    }
}

/// The `flags` argument of a PMS query (`long` on API 34, `int` on older
/// signatures): `0` when the caller supplied none.
fn flags_arg(args: &[Value], index: usize) -> i64 {
    match args.get(index) {
        Some(Value::Long(flags)) => *flags,
        Some(Value::Int(flags)) => i64::from(*flags),
        _ => 0,
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
    /// The APK's signing certificates (DER X.509), as
    /// `PackageInfo.signatures` carries them under `GET_SIGNATURES`.
    pub signatures: Vec<Vec<u8>>,
}

impl PackageInfo {
    /// Encode in the full `PackageInfo.writeToParcel` order (41 fields).
    ///
    /// `PackageParser.generatePackageInfo` builds the nested `ApplicationInfo`
    /// with the same request flags, so the metadata `Bundle` follows them.
    #[must_use]
    pub fn to_parcelable_with_flags(&self, flags: i64) -> Parcelable {
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
                    values: vec![Value::Parcelable(
                        self.application.to_parcelable_with_flags(flags),
                    )],
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
                // `PackageInfo.signatures`: `Signature[]`, each element a
                // `writeByteArray` (the image's `Signature.writeToParcel`).
                Value::List(Some(
                    self.signatures
                        .iter()
                        .map(|der| {
                            Value::Parcelable(Parcelable::ordered(
                                "android.content.pm.Signature",
                                vec![Value::ByteArray(Some(der.clone()))],
                            ))
                        })
                        .collect(),
                )),
                Value::List(None),  // configPreferences
                Value::List(None),  // reqFeatures
                Value::List(None),  // featureGroups
                Value::List(None),  // attributions
                Value::Int(0),      // installLocation
                Value::Int(0),      // isStub
                Value::Int(0),      // coreApp
                Value::Int(0),      // requiredForAllUsers
                Value::Str8(None),  // restrictedAccountType
                Value::Str8(None),  // requiredAccountType
                Value::Str8(None),  // overlayTarget
                Value::Str8(None),  // overlayCategory
                Value::Int(0),      // overlayPriority
                Value::Bool(false), // mOverlayIsStatic
                Value::Int(0),      // compileSdkVersion
                Value::Str8(None),  // compileSdkVersionCodename
                Value::Cond {
                    present: false,
                    values: Vec::new(),
                }, // signingInfo
                Value::Bool(false), // isApex
                Value::Bool(false), // isActiveApex
            ],
        )
    }

    /// Encode without the metadata `Bundle` (the flags-off form).
    #[must_use]
    pub fn to_parcelable(&self) -> Parcelable {
        self.to_parcelable_with_flags(0)
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
        // `android.software.webview` is deliberately absent from this static
        // table: it is not a device constant but follows from whether the
        // session has a WebView provider package installed (`PmsState::
        // has_system_feature`, ADR-0031).
        for (name, enabled) in [
            ("android.hardware.touchscreen", true),
            ("android.hardware.touchscreen.multitouch", true),
            ("android.hardware.touchscreen.multitouch.distinct", true),
            ("android.hardware.faketouch", true),
            ("android.hardware.type.watch", false),
            ("android.hardware.vulkan.version", false),
            ("android.software.companion_device_setup", false),
            ("android.software.picture_in_picture", false),
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

    /// Every feature the table declares, in declaration order.
    ///
    /// `android.software.webview` is not in the table: its answer is a package
    /// fact ([`PmsState::has_system_feature`]), so it never appears here.
    #[must_use]
    pub fn declared(&self) -> Vec<(&str, bool)> {
        self.features
            .iter()
            .map(|(name, enabled)| (name.as_str(), *enabled))
            .collect()
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

// ------------------------------------------------------------ manifest labels

/// A `PackageItemInfo` label exactly as `PackageParser` stores it.
///
/// The framework never resolves a label to text at install time: it keeps the
/// two fields `PackageItemInfo.loadLabel` reads — `nonLocalizedLabel` for a
/// literal `android:label` string and `labelRes` for `android:label="@string/x"
/// (the two are mutually exclusive; `labelRes` is 0 when a literal was given).
/// Stock `ActivityThread.performLaunchActivity` then calls
/// `r.activityInfo.loadLabel(pm)` for the window title, which walks
/// activity label -> application label -> component name. A guest that leaves
/// both fields unset therefore shows the component FQN as the ActionBar title.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelSpec {
    /// `PackageItemInfo.labelRes` (0 = no resource label).
    pub res: i32,
    /// `PackageItemInfo.nonLocalizedLabel` (a literal `android:label` value).
    pub literal: Option<String>,
}

impl LabelSpec {
    /// `ParsingPackageUtils.parseBaseApplication`/`parseActivity` semantics:
    /// a literal string wins and clears the resource id; otherwise the
    /// resource reference is kept; neither leaves both unset.
    #[must_use]
    pub fn from_parts(literal: Option<&String>, res: Option<u32>) -> Self {
        match (literal, res) {
            (Some(text), _) => Self {
                res: 0,
                literal: Some(text.clone()),
            },
            (None, Some(id)) => Self {
                res: id as i32,
                literal: None,
            },
            (None, None) => Self::default(),
        }
    }

    /// True when the component has no label of its own, so `loadLabel` falls
    /// through to `applicationInfo.loadLabel(pm)`.
    #[must_use]
    pub fn is_unset(&self) -> bool {
        self.res == 0 && self.literal.is_none()
    }
}

/// `PackageParser`'s label values for an APK: the application plus every
/// declared activity (fully-qualified class name).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestLabels {
    /// `<application android:label>`.
    pub application: LabelSpec,
    /// `(fully-qualified activity class, <activity android:label>)`, declaration
    /// order. An activity without its own label has an unset [`LabelSpec`].
    pub activities: Vec<(String, LabelSpec)>,
}

impl ManifestLabels {
    /// The label spec for a fully-qualified activity name, or an unset spec when
    /// the APK does not declare it (stock falls back to the application label).
    #[must_use]
    pub fn activity(&self, fq_name: &str) -> LabelSpec {
        self.activities
            .iter()
            .find(|(name, _)| name == fq_name)
            .map(|(_, spec)| spec.clone())
            .unwrap_or_default()
    }
}

/// Read the label specs `PackageParser` would write into `ApplicationInfo` and
/// `ActivityInfo` for an APK. The manifest is the same one [`PmsState`] installs.
///
/// Served from [`crate::apk_facts`], so a launch that also resolves the launcher
/// does not open the APK again.
pub fn manifest_labels(apk: impl AsRef<Path>) -> Result<ManifestLabels> {
    let path = apk.as_ref();
    let facts = crate::apk_facts::launch_facts(path)
        .map_err(|e| ServiceError::Internal(format!("open {}: {e}", path.display())))?;
    facts.labels.clone().map_err(ServiceError::Internal)
}

/// The label values of an already-parsed manifest (see [`manifest_labels`]).
pub(crate) fn labels_from_manifest(manifest: &acos_apk::manifest::Manifest) -> ManifestLabels {
    let application = LabelSpec::from_parts(
        manifest.application.label.as_ref(),
        manifest.application.label_ref,
    );
    let activities = manifest
        .activities
        .iter()
        .map(|c| {
            (
                acos_apk::manifest::fq_name(&manifest.package, &c.name),
                LabelSpec::from_parts(c.label.as_ref(), c.label_ref),
            )
        })
        .collect();
    ManifestLabels {
        application,
        activities,
    }
}

/// One installed package: everything PMS knows about it.
///
/// A session installs the app under test (`primary`) plus any extra APK given
/// at launch, and the packages baked into the image. AOSP gives every package
/// its own uid (`PackageManagerService` allocates one per package unless a
/// shared `android:sharedUserId` says otherwise), so the uid lives on the
/// record.
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledPackage {
    /// The application record (`ApplicationInfo` source).
    pub app: AppInfo,
    /// The package record (activities, version, ...).
    pub package: PackageInfo,
    /// Declared content providers (`<provider>` + `android:authorities`).
    pub providers: Vec<ProviderInfo>,
    /// Declared services (`<service>`).
    pub services: Vec<ServiceInfo>,
    /// Granted permissions (manifest + install `-g`).
    pub granted_permissions: Vec<String>,
    /// `<permission>` elements the package declares itself (SYSUI3B): the
    /// `PermissionInfo` an `IPermissionManager.getPermissionInfo` for a custom
    /// permission answers comes from here.
    pub declared_permissions: Vec<DeclaredPermission>,
    /// installer package (null = side-loaded).
    pub installer_package: Option<String>,
    /// `sourceDir` the state was built from.
    pub apk_path: PathBuf,
    /// The reduced manifest (`ams::PackageRecord`): the intent filters the
    /// package declared. `queryIntentActivities`/`resolveIntent` match against
    /// this table, the same one ATMS resolves `startActivity` with.
    pub manifest: crate::ams::PackageRecord,
}

impl InstalledPackage {
    /// Build one package record from an APK that is already open. `uid` is the
    /// package's own uid (`PackageManagerService` allocates one per package).
    pub fn from_opened(path: impl AsRef<Path>, apk: &acos_apk::Apk, uid: i32) -> Result<Self> {
        let path = path.as_ref();
        let manifest = apk
            .manifest()
            .map_err(|e| ServiceError::Internal(format!("manifest: {e}")))?;
        let app = AppInfo::from_manifest(&manifest, path, uid);
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
                meta_data: c.meta_data.clone(),
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
                    // `androidx.core.content.FileProvider.attachInfo` throws
                    // `SecurityException("Provider must grant uri permissions")`
                    // when the provider is exported and this manifest bit is
                    // absent; it is `android:grantUriPermissions`, default false.
                    grant_uri_permissions: c.grant_uri_permissions.unwrap_or(false),
                    init_order: 0,
                    meta_data: c.meta_data.clone(),
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
                    meta_data: c.meta_data.clone(),
                }
            })
            .collect();
        let version_code = manifest.version_code.unwrap_or(1) as i32;
        let version_name = manifest
            .version_name
            .clone()
            .unwrap_or_else(|| "1.0".into());
        let package = PackageInfo {
            package_name: app.package_name.clone(),
            version_code,
            version_name,
            application: app.clone(),
            activities,
            requested_permissions: requested_permissions.clone(),
            first_install_time: 0,
            last_update_time: 0,
            signatures: apk.signing_certificates.clone(),
        };
        Ok(Self {
            app,
            package,
            providers,
            services,
            granted_permissions: requested_permissions,
            declared_permissions: manifest.permissions.clone(),
            installer_package: None,
            apk_path: path.to_path_buf(),
            manifest: crate::ams::PackageRecord::from_manifest(&manifest, uid),
        })
    }

    /// Publish `guest` as this package's `sourceDir`/`publicSourceDir`.
    ///
    /// The record is built from the host file, but every
    /// `ApplicationInfo`/`PackageInfo` reply must name the path the caller can
    /// open. Both copies of the record (`app` and `package.application`) are
    /// updated: the parcelable hierarchy nests the second one in every
    /// `ComponentInfo` reply.
    pub fn set_guest_apk_path(&mut self, guest: &str) {
        for app in [&mut self.app, &mut self.package.application] {
            app.source_dir = guest.to_string();
            app.public_source_dir = guest.to_string();
        }
    }

    /// Publish `guest` as the installed package's `nativeLibraryDir`.
    ///
    /// MPROC1: an extra APK's libraries are installed into a **per-package**
    /// guest tree (`/data/app-lib-extra/<package>/<abi>`), so two packages that
    /// ship a library with the same name can never shadow each other. The path
    /// is the one `LoadedApk` puts on the class loader's native search path,
    /// exactly as for the launched package.
    pub fn set_guest_native_lib_dir(&mut self, guest: &str) {
        for app in [&mut self.app, &mut self.package.application] {
            app.native_library_dir = guest.to_string();
        }
    }
}

/// The installed-package table of one session.
#[derive(Debug, Clone)]
pub struct PmsState {
    /// Every installed package; `primary` is the app under test.
    pub packages: Vec<InstalledPackage>,
    /// Index of the package under test. A `None`/`""` package lookup resolves
    /// here (the framework asks `getApplicationInfo(null)` for the calling
    /// process's own package).
    pub primary: usize,
    /// Feature answers.
    pub features: FeatureTable,
    /// The one runtime-permission table, shared with the ATMS and the
    /// `permissions` module (MODNOTIF2).
    pub runtime_permissions:
        Option<std::sync::Arc<std::sync::Mutex<crate::permissions::PermissionStore>>>,
    /// uid -> package.
    pub uid_packages: Vec<(i32, String)>,
    /// dex-load notifications (oneway, recorded) as
    /// `(package, loaderIsa, classLoaderContextMap size)`.
    pub dex_loads: Vec<(String, String, usize)>,
    /// `setComponentEnabledSetting` overrides, keyed by
    /// `(package, class)` → `COMPONENT_ENABLED_STATE_*` (W32).
    ///
    /// The manifest's own `android:enabled` is **not** in here: on a device it
    /// is `ActivityInfo.enabled`/`ServiceInfo.enabled`, not a component
    /// *setting*, and `PackageManager.getComponentEnabledSetting` reports
    /// `COMPONENT_ENABLED_STATE_DEFAULT` (0) until an app calls the setter.
    pub component_settings: Vec<((String, String), i32)>,
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
                meta_data: Vec::new(),
            }],
            requested_permissions: Vec::new(),
            first_install_time: 0,
            last_update_time: 0,
            signatures: Vec::new(),
        };
        let manifest = crate::ams::PackageRecord {
            package: app.package_name.clone(),
            uid: app.uid,
            activities: vec![crate::ams::DeclaredComponent {
                package: app.package_name.clone(),
                class: app.class_name.clone(),
                exported: true,
                launch_mode: 0,
                filters: Vec::new(),
            }],
            ..crate::ams::PackageRecord::default()
        };
        let uid_packages = vec![(app.uid, app.package_name.clone())];
        Self {
            packages: vec![InstalledPackage {
                app,
                package,
                providers: Vec::new(),
                services: Vec::new(),
                granted_permissions: vec!["android.permission.INTERNET".into()],
                declared_permissions: Vec::new(),
                installer_package: None,
                apk_path: apk,
                manifest,
            }],
            primary: 0,
            features: FeatureTable::default(),
            runtime_permissions: None,
            uid_packages,
            dex_loads: Vec::new(),
            component_settings: Vec::new(),
        }
    }

    /// Build from an APK on disk.
    pub fn from_apk(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        // PERF-REL3c: the shared open, so a launch already holding it does not read
        // the file again.
        let apk = crate::apk_facts::open_shared(path)
            .map_err(|e| ServiceError::Internal(format!("open {}: {e}", path.display())))?;
        Self::from_opened(path, &apk)
    }

    /// [`PmsState::from_apk`] for a package whose uid the package manager has
    /// **already** allocated (AOSP allocates one uid per installed package).
    ///
    /// A package's `ApplicationInfo.uid` must be its own: building an extra
    /// package's facts with the fixture uid would describe a different app's uid
    /// while nothing else about the record changed.
    pub fn from_apk_with_uid(path: impl AsRef<Path>, uid: i32) -> Result<Self> {
        let path = path.as_ref();
        let apk = crate::apk_facts::open_shared(path)
            .map_err(|e| ServiceError::Internal(format!("open {}: {e}", path.display())))?;
        Self::from_opened_with_uid(path, &apk, uid)
    }

    /// [`PmsState::from_opened`] for an APK that is already open and a package
    /// uid the caller owns (see [`PmsState::from_apk_with_uid`]).
    pub fn from_opened_with_uid(path: impl AsRef<Path>, apk: &acos_apk::Apk, uid: i32) -> Result<Self> {
        let record = InstalledPackage::from_opened(path, apk, uid)?;
        let uid_packages = vec![(record.app.uid, record.app.package_name.clone())];
        Ok(Self {
            packages: vec![record],
            primary: 0,
            features: FeatureTable::default(),
            runtime_permissions: None,
            uid_packages,
            dex_loads: Vec::new(),
            component_settings: Vec::new(),
        })
    }

    /// [`PmsState::from_apk`] for an APK that is already open (`apk` is the open of
    /// `path`). PERF-REL3c: a launch reads its source APK once.
    pub fn from_opened(path: impl AsRef<Path>, apk: &acos_apk::Apk) -> Result<Self> {
        Self::from_opened_with_uid(path, apk, FIXTURE_UID)
    }

    /// Install an extra package into this session: an APK given at launch, or a
    /// package baked into the image. It gets the next free uid (AOSP allocates
    /// one uid per package) and is reachable by every package query.
    ///
    /// Installing the same package name twice is a structured error: the
    /// framework's own `PackageManager` replaces on reinstall, but a session
    /// install is explicit and a silent replace would hide a caller bug.
    pub fn install_extra(
        &mut self,
        path: impl AsRef<Path>,
        guest_source_dir: Option<&str>,
    ) -> Result<()> {
        self.install_extra_with(path, guest_source_dir, None)
            .map(|_| ())
    }

    /// [`Self::install_extra`] with the package's own `nativeLibraryDir`, and
    /// the uid the install assigned.
    ///
    /// MPROC1: the launcher extracts an extra APK's `lib/<abi>` into a
    /// per-package tree and must publish that guest path on the package, so an
    /// `ApplicationInfo`/`PackageInfo` reply for it names a directory the guest
    /// can open (`LoadedApk`'s native search path). The uid is the one the
    /// activity manager's manifest table is seeded with for the same package.
    pub fn install_extra_with(
        &mut self,
        path: impl AsRef<Path>,
        guest_source_dir: Option<&str>,
        guest_native_lib_dir: Option<&str>,
    ) -> Result<i32> {
        let path = path.as_ref();
        let apk = crate::apk_facts::open_shared(path)
            .map_err(|e| ServiceError::Internal(format!("open {}: {e}", path.display())))?;
        let mut record = InstalledPackage::from_opened(path, &apk, self.next_uid())?;
        if let Some(guest) = guest_source_dir {
            record.set_guest_apk_path(guest);
        }
        if let Some(guest) = guest_native_lib_dir {
            record.set_guest_native_lib_dir(guest);
        }
        if self
            .packages
            .iter()
            .any(|p| p.package.package_name == record.package.package_name)
        {
            return Err(ServiceError::BadRequest {
                interface: "android.content.pm.IPackageManager".into(),
                method: "installPackage".into(),
                detail: format!(
                    "package {} is already installed in this session",
                    record.package.package_name
                ),
            });
        }
        let uid = record.app.uid;
        self.uid_packages
            .push((uid, record.app.package_name.clone()));
        self.packages.push(record);
        // BINDERPID: the uid -> package/grant table the per-uid permission check
        // reads must learn about the new package at the same moment the uid
        // table does, or `checkCallingPermission` from that package's process
        // answers from the wrong app's grants.
        self.publish_uid_grants();
        Ok(uid)
    }

    /// The next free uid for a new package.
    fn next_uid(&self) -> i32 {
        let mut uid = FIXTURE_UID;
        while self.uid_packages.iter().any(|(u, _)| *u == uid) {
            uid += 1;
        }
        uid
    }

    /// The package under test.
    #[must_use]
    pub fn primary(&self) -> &InstalledPackage {
        &self.packages[self.primary]
    }

    /// The package under test, mutably.
    pub fn primary_mut(&mut self) -> &mut InstalledPackage {
        &mut self.packages[self.primary]
    }

    /// The installed application of the package under test.
    #[must_use]
    pub fn app(&self) -> &AppInfo {
        &self.primary().app
    }

    /// The package record of the package under test.
    #[must_use]
    pub fn package(&self) -> &PackageInfo {
        &self.primary().package
    }

    /// The declared providers of the package under test.
    #[must_use]
    pub fn providers(&self) -> &[ProviderInfo] {
        &self.primary().providers
    }

    /// The installed record for `package`. `None`/`""` is the package under
    /// test; an unknown name is `None` (the wire contract answers a null
    /// `ApplicationInfo`/`PackageInfo` and the client raises
    /// `NameNotFoundException`).
    #[must_use]
    pub fn lookup(&self, package: Option<&str>) -> Option<&InstalledPackage> {
        match package {
            None | Some("") => self.packages.get(self.primary),
            Some(name) => self
                .packages
                .iter()
                .find(|r| r.package.package_name == name),
        }
    }

    /// `PackageManager.hasSystemFeature(name)`.
    ///
    /// Almost every name is a constant of the device profile. One is not:
    /// `android.software.webview` is true exactly when a WebView provider
    /// package is installed in this session (ADR-0031 — the provider is an
    /// ordinary system package, so the feature follows from the package table,
    /// not from a flag). That is also what the framework then checks next:
    /// `WebViewFactory.getProvider()` throws `UnsupportedOperationException`
    /// when this is false, exactly as on a phone without a provider.
    #[must_use]
    pub fn has_system_feature(&self, name: &str) -> bool {
        if name == crate::webview::WEBVIEW_FEATURE {
            return self.has_webview_provider();
        }
        self.features.has(name)
    }

    /// Is a WebView provider package installed in this session?
    ///
    /// The provider is a system package baked into the session image, so the
    /// test is the same package-table lookup any other package gets.
    #[must_use]
    pub fn has_webview_provider(&self) -> bool {
        self.lookup(Some(crate::webview::PROVIDER_PACKAGE))
            .is_some()
    }

    /// The installed record and provider declaring `authority`, if any.
    #[must_use]
    pub fn provider_for_authority(
        &self,
        authority: &str,
    ) -> Option<(&InstalledPackage, &ProviderInfo)> {
        self.packages.iter().find_map(|record| {
            record
                .providers
                .iter()
                .find(|p| p.authority == authority)
                .map(|p| (record, p))
        })
    }

    /// Publish `guest` as the installed APK's `sourceDir`/`publicSourceDir`.
    pub fn set_guest_apk_path(&mut self, guest: &str) {
        self.primary_mut().set_guest_apk_path(guest);
    }

    /// Locate and load the fixture APK, falling back to the baked-in values.
    #[must_use]
    pub fn hello_views() -> Self {
        let path = crate::workspace_path("fixtures/build/apps/hello-views.apk")
            .unwrap_or_else(|| PathBuf::from("fixtures/build/apps/hello-views.apk"));
        Self::from_apk(&path).unwrap_or_else(|_| Self::new())
    }

    /// `PackageManager.checkPermission` policy: the shared runtime table first,
    /// then the manifest's install-time grants.
    #[must_use]
    pub fn check_permission(&self, permission: &str) -> i32 {
        self.check_permission_for(None, permission)
    }

    /// [`PmsState::check_permission`] for a **named** package (MPROC1: the
    /// question is about the package the caller names, not the one under test).
    ///
    /// The runtime table is keyed by permission name and describes one package
    /// (ACOS installs one app per session). It answers for the package it was
    /// built for — and for a package name this session has no record of at all:
    /// there is exactly one runtime table, so a name the session cannot resolve
    /// cannot have a second one, and answering `-1` there is what made the PMS
    /// and the ATMS disagree about the session's one app (MODNOTIF2). A name that
    /// **is** installed is answered from its own manifest record, so a runtime
    /// grant made for the app under test never leaks to a second package.
    ///
    /// This is **the** answer both `IPackageManager.checkPermission` and the
    /// in-process accessors use (MODNOTIF2: one table, so the activity manager's
    /// and the package manager's `checkPermission` can never disagree).
    #[must_use]
    pub fn check_permission_for(&self, package: Option<&str>, permission: &str) -> i32 {
        const GRANTED: i32 = crate::permissions::PERMISSION_GRANTED;
        const DENIED: i32 = crate::permissions::PERMISSION_DENIED;
        let record = self.lookup(package);
        if let Some(store) = self.runtime_permissions.as_ref() {
            let store = store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let describes_it = match record {
                Some(record) => store.package == record.package.package_name,
                None => true,
            };
            if describes_it && store.entry(permission).is_some() {
                return store.check(permission);
            }
        }
        let Some(record) = record else {
            // No record and no runtime answer: the permission is not granted to
            // anything this session knows.
            return DENIED;
        };
        if record.granted_permissions.iter().any(|p| p == permission) {
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

    /// `IPackageManager.getNameForUid(int uid)`: the package the uid belongs
    /// to, or `None` (which the wire writes as a null string).
    ///
    /// AOSP answers the first package of the uid
    /// (`PackageManagerService.getNameForUid`) and null for a uid no package
    /// owns. The framework's permission check path asks this to turn a calling
    /// uid into a package (`ActivityManager.checkComponentPermission` ->
    /// `ContextImpl.checkComponentPermission`), so the answer must be the same
    /// uid table `getPackagesForUid` and the ATMS records use.
    #[must_use]
    pub fn name_for_uid(&self, uid: i32) -> Option<String> {
        self.packages_for_uid(uid).into_iter().next()
    }

    /// Every installed package's `(uid, package, manifest grants)`.
    ///
    /// This is what the one permission table needs to answer
    /// `IPermissionManager.checkUidPermission` (the framework's
    /// `Context.checkCallingPermission` path): the question is asked by **uid**,
    /// and only the package manager can say which package (and therefore which
    /// install-time grant set) a uid names.
    #[must_use]
    pub fn uid_grants(&self) -> Vec<(i32, String, Vec<String>)> {
        self.packages
            .iter()
            .map(|p| {
                (
                    p.app.uid,
                    p.app.package_name.clone(),
                    p.granted_permissions.clone(),
                )
            })
            .collect()
    }

    /// Republish [`Self::uid_grants`] into the session's permission table.
    ///
    /// Called whenever the installed set changes (first install, extra package):
    /// the table is shared with `ATMS`, `core`'s `IPermissionManager` and the
    /// `system.permissions` module, so one publish keeps all of them answering
    /// per-uid questions from the same data.
    fn publish_uid_grants(&self) {
        let Some(store) = self.runtime_permissions.as_ref() else {
            return;
        };
        if let Ok(mut store) = store.lock() {
            store.set_uid_grants(self.uid_grants());
        }
    }

    /// `COMPONENT_ENABLED_STATE_DEFAULT` (the stock value of
    /// `PackageManager.COMPONENT_ENABLED_STATE_DEFAULT`).
    pub const COMPONENT_ENABLED_STATE_DEFAULT: i32 = 0;
    /// `COMPONENT_ENABLED_STATE_ENABLED`.
    pub const COMPONENT_ENABLED_STATE_ENABLED: i32 = 1;
    /// `COMPONENT_ENABLED_STATE_DISABLED`.
    pub const COMPONENT_ENABLED_STATE_DISABLED: i32 = 2;
    /// `COMPONENT_ENABLED_STATE_DISABLED_USER`.
    pub const COMPONENT_ENABLED_STATE_DISABLED_USER: i32 = 3;
    /// `COMPONENT_ENABLED_STATE_DISABLED_UNTIL_USED`.
    pub const COMPONENT_ENABLED_STATE_DISABLED_UNTIL_USED: i32 = 4;

    /// `PackageManager.getComponentEnabledSetting(ComponentName, userId)`.
    ///
    /// ACOS installs one package for one user (`uid` on the fixture), so the
    /// userId is accepted and not part of the key: a setting recorded for the
    /// installed package is the answer for the installed app. A component that
    /// was never set answers `COMPONENT_ENABLED_STATE_DEFAULT`, exactly as
    /// `PackageManagerService.getComponentEnabledSetting` does.
    #[must_use]
    pub fn component_setting(&self, package: &str, class: &str) -> i32 {
        self.component_settings
            .iter()
            .find(|((p, c), _)| p == package && c == class)
            .map(|(_, state)| *state)
            .unwrap_or(Self::COMPONENT_ENABLED_STATE_DEFAULT)
    }

    /// Record a `setComponentEnabledSetting` override. Returns `false` (and
    /// changes nothing) when `new_state` is not one of the five stock values
    /// `PackageManagerService` accepts — the framework throws
    /// `IllegalArgumentException` for those, so the host must not store
    /// something the getter could then report as a valid setting.
    pub fn set_component_setting(&mut self, package: &str, class: &str, new_state: i32) -> bool {
        if !(Self::COMPONENT_ENABLED_STATE_DEFAULT
            ..=Self::COMPONENT_ENABLED_STATE_DISABLED_UNTIL_USED)
            .contains(&new_state)
        {
            return false;
        }
        match self
            .component_settings
            .iter_mut()
            .find(|((p, c), _)| p == package && c == class)
        {
            Some((_, state)) => *state = new_state,
            None => self
                .component_settings
                .push(((package.to_string(), class.to_string()), new_state)),
        }
        true
    }
}

impl Default for PmsState {
    fn default() -> Self {
        Self::new()
    }
}

/// `ComponentName` (`package`, `class`) from a decoded parcelable argument.
///
/// The image's `ComponentName.writeToParcel` writes `mPackage` then `mClass`,
/// which is also what `parcelable.rs`'s spec records; a missing argument is an
/// empty pair, so the getter answers DEFAULT instead of failing.
fn component_parts(component: Option<&Parcelable>) -> (String, String) {
    let package = component
        .and_then(|p| p.field(0))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let class = component
        .and_then(|p| p.field(1))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (package, class)
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
    wiring: crate::host::Wiring,
}

impl PmsService {
    /// Publish (or withdraw) one `hasSystemFeature` answer.
    ///
    /// A service module that makes a system feature real turns it on at attach
    /// and off at detach, so `hasSystemFeature` cannot disagree with the
    /// attached module set.
    pub fn set_system_feature(&mut self, name: &str, enabled: bool) -> crate::error::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.features.set(name, enabled);
        Ok(())
    }

    /// Wrap shared PMS state.
    #[must_use]
    pub fn new(state: Arc<Mutex<PmsState>>, wiring: crate::host::Wiring) -> Self {
        Self { state, wiring }
    }

    /// Replace the installed package with the APK being launched.
    ///
    /// `ServiceHost::standard()` seeds the PMS with the `hello-views` fixture;
    /// the launch path re-points it at the real APK before the framework asks
    /// for `getApplicationInfo`. A missing APK is a structured error, never a
    /// silent fallback to the previous package.
    pub fn set_package_from_apk(&mut self, apk_path: &Path) -> Result<()> {
        self.set_package_from_apk_as(apk_path, None)
    }

    /// The shared PMS state (MPROC7: the peer resolves the `ApplicationInfo` and
    /// the `ActivityInfo` of a launch the host encodes itself).
    #[must_use]
    pub fn shared_state(&self) -> Arc<Mutex<PmsState>> {
        Arc::clone(&self.state)
    }

    /// The same install with the APK path the **guest** uses.
    ///
    /// `sourceDir`/`publicSourceDir` are what
    /// `PackageManager.getResourcesForApplication` passes to
    /// `ResourcesManager.getResources`, so the value the PMS publishes has to be
    /// a path the caller can open. In the confined ART guest that is
    /// `/data/app/<name>.apk` (the APK is read-only-bound there), not the host
    /// file the parser read: publishing the host path made every
    /// `PackageItemInfo.loadXmlMetaData` (`FileProvider`'s
    /// `FILE_PROVIDER_PATHS` XML) fail with
    /// `ApkAssets.nativeLoad: Failed to load asset path <host path>`.
    pub fn set_package_from_apk_as(
        &mut self,
        apk_path: &Path,
        guest_source_dir: Option<&str>,
    ) -> Result<()> {
        let mut state = PmsState::from_apk(apk_path)?;
        if let Some(guest) = guest_source_dir {
            state.set_guest_apk_path(guest);
        }
        // The session's one permission table outlives a package replacement: it
        // is the table `ATMS`, `core`'s `IPermissionManager` and the
        // `system.permissions` module all read (MODNOTIF2), and BINDERPID's
        // per-uid check reads its uid -> package/grants snapshot. Replacing the
        // package state without re-publishing would leave both describing the
        // *previous* APK, which is exactly the kind of silent disagreement the
        // one table exists to prevent.
        //
        // SYSUI1: the runtime-permission table describes the session's one app,
        // and the in-process package manager is seeded with the `hello-views`
        // fixture before boot. Binding the table's package identity only at the
        // first `ensure("package")` (registry) left it named after the fixture
        // forever, so a permission dialog answered for the wrong app. The install
        // of the real APK is the moment the identity is knowable, so the table is
        // re-pointed here, before any guest can ask.
        let mut guard = self.state.lock().unwrap();
        state.runtime_permissions = guard.runtime_permissions.clone();
        state.publish_uid_grants();
        if let Some(store) = state.runtime_permissions.clone() {
            let primary = state.primary();
            let (package, granted, declared) = (
                primary.package.package_name.clone(),
                primary.granted_permissions.clone(),
                primary.declared_permissions.clone(),
            );
            let mut store = store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            store.set_install_time(&package, &granted);
            store.set_declared_permissions(&package, &declared);
            store.set_uid_grants(state.uid_grants());
        }
        *guard = state;
        Ok(())
    }

    /// MPROC1: install **one more** package into this session without touching
    /// the packages already installed. Returns the uid the install assigned.
    ///
    /// The launched APK is installed first ([`Self::set_package_from_apk_as`],
    /// which replaces the session state); every extra APK then goes through
    /// here, so installing the second package cannot drop the first.
    pub fn install_extra_package_from_apk_as(
        &mut self,
        apk_path: &Path,
        guest_source_dir: Option<&str>,
        guest_native_lib_dir: Option<&str>,
    ) -> Result<i32> {
        self.state.lock().unwrap().install_extra_with(
            apk_path,
            guest_source_dir,
            guest_native_lib_dir,
        )
    }

    /// The installed record for `package`. `None`/`""` is the package under
    /// test; an unknown name is the wire's null reply (the client raises
    /// `NameNotFoundException` itself, K3b), so it is not a service error.
    fn lookup<'a>(state: &'a PmsState, package: Option<&str>) -> Result<&'a InstalledPackage> {
        state
            .lookup(package)
            .ok_or_else(|| ServiceError::UnknownService {
                name: package.unwrap_or_default().to_string(),
            })
    }

    /// Record the resolved package for the materialiser.
    fn cache(record: &InstalledPackage) {
        cache_description(PmsDescription {
            app: record.app.clone(),
            package: record.package.clone(),
        });
    }
}

impl ServiceObject for PmsService {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.content.pm.IPackageManager" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let mut state = self.state.lock().unwrap();
        // The materialiser reads the package of the *resolved* record; this
        // seeds it with the package under test and every branch that resolves
        // a package overwrites it before the reply is materialised.
        Self::cache(state.primary());
        let reply = match method.name.as_str() {
            // K3b: an unknown package is **not** an error on the wire. The
            // image's `IPackageManager` replies with a null `ApplicationInfo`/
            // `PackageInfo` and the client (`ApplicationPackageManager`,
            // `getApplicationInfoAsUser`/`getPackageInfoAsUser`) raises
            // `PackageManager.NameNotFoundException` itself. Sending a parcel
            // exception here made the app see `IllegalArgumentException`
            // instead of the exception it catches, which is what stopped vlc on
            // `getPackageInfo("com.google.android.gsf")` (K2 §6).
            // K3d: the `flags` argument (a `long` on API 34) is honoured for
            // `GET_META_DATA` on every `PackageItemInfo` reply, exactly as
            // `PackageParser.generate*Info` does; without the flag the
            // `metaData` field is null.
            "getApplicationInfo" => {
                let package = args.first().and_then(Value::as_str);
                let flags = flags_arg(args, 1);
                let info = match Self::lookup(&state, package) {
                    Ok(record) => {
                        trace_query(
                            "getApplicationInfo",
                            package.unwrap_or(""),
                            flags,
                            record.app.meta_data.len(),
                        );
                        Self::cache(record);
                        record.app.to_parcelable_with_flags(flags)
                    }
                    Err(ServiceError::UnknownService { .. }) => Parcelable::Null,
                    Err(e) => return Err(e),
                };
                vec![Value::TypedObject(info)]
            }
            "getPackageInfo" => {
                let package = args.first().and_then(Value::as_str);
                let flags = flags_arg(args, 1);
                trace_query(
                    "getPackageInfo",
                    package.unwrap_or(""),
                    flags,
                    state.package().requested_permissions.len(),
                );
                let info = match Self::lookup(&state, package) {
                    Ok(record) => {
                        Self::cache(record);
                        let value = record.package.to_parcelable_with_flags(flags);
                        dump_reply(
                            &format!("pms.getPackageInfo.{}", record.package.package_name),
                            &value,
                        );
                        value
                    }
                    Err(ServiceError::UnknownService { .. }) => Parcelable::Null,
                    Err(e) => return Err(e),
                };
                vec![Value::TypedObject(info)]
            }
            "getActivityInfo" => {
                let component = args.first().and_then(Value::as_parcelable);
                // `ComponentName.writeToParcel` writes `mPackage` then `mClass`.
                let (component_package, name) = component_parts(component);
                let record = state.lookup(Some(&component_package));
                let activity = record.and_then(|r| {
                    r.package
                        .activities
                        .iter()
                        .find(|a| a.name == name)
                        .cloned()
                });
                cache_activity_info(activity.clone().map(|a| {
                    (
                        record.map_or_else(|| state.primary().app.clone(), |r| r.app.clone()),
                        a,
                    )
                }));
                let flags = flags_arg(args, 1);
                vec![match (activity, record) {
                    (Some(a), Some(r)) => {
                        Self::cache(r);
                        Value::TypedObject(a.to_parcelable_with_flags(&r.app, flags))
                    }
                    _ => Value::TypedObject(Parcelable::Null),
                }]
            }
            "getServiceInfo" => {
                // `getServiceInfo(ComponentName, long flags, int userId)`.
                let component = args.first().and_then(Value::as_parcelable);
                let (component_package, name) = component_parts(component);
                let record = state.lookup(Some(&component_package));
                let service =
                    record.and_then(|r| r.services.iter().find(|s| s.name == name).cloned());
                let flags = flags_arg(args, 1);
                vec![match (service, record) {
                    (Some(s), Some(r)) => {
                        Self::cache(r);
                        Value::TypedObject(s.to_parcelable_with_flags(&r.app, flags))
                    }
                    _ => Value::TypedObject(Parcelable::Null),
                }]
            }
            "getProviderInfo" => {
                // `getProviderInfo(ComponentName, long flags, int userId)`.
                let component = args.first().and_then(Value::as_parcelable);
                let (component_package, name) = component_parts(component);
                let record = state.lookup(Some(&component_package));
                let provider =
                    record.and_then(|r| r.providers.iter().find(|p| p.name == name).cloned());
                let flags = flags_arg(args, 1);
                trace_query(
                    "getProviderInfo",
                    &name,
                    flags,
                    provider.as_ref().map_or(0, |p| p.meta_data.len()),
                );
                vec![match (provider, record) {
                    (Some(p), Some(r)) => {
                        Self::cache(r);
                        Value::TypedObject(p.to_parcelable_with_flags(&r.app, flags))
                    }
                    _ => Value::TypedObject(Parcelable::Null),
                }]
            }
            "getPackagesForUid" => {
                let uid = args.first().and_then(Value::as_int).unwrap_or(FIXTURE_UID);
                let packages = state.packages_for_uid(uid);
                vec![Value::StringArray(Some(
                    packages.into_iter().map(Some).collect(),
                ))]
            }
            // `checkUidPermission(String permission, int uid) -> int`: the
            // permission answer **for a uid**, which is what the framework's
            // `ActivityManager.checkComponentPermission` (and therefore
            // `Context.checkCallingPermission`) asks. Answered from the one
            // shared table, so it cannot disagree with
            // `IPermissionManager.checkUidPermission` or `checkPermission`.
            "checkUidPermission" => {
                let permission = args.first().and_then(Value::as_str).unwrap_or_default();
                let uid = args.get(1).and_then(Value::as_int).unwrap_or(0);
                let answer = state
                    .runtime_permissions
                    .as_ref()
                    .map(|store| {
                        let store = store
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        store.check_uid_permission(uid as u32, permission)
                    })
                    .unwrap_or_else(|| state.check_permission_for(None, permission));
                vec![Value::Int(answer)]
            }
            // `getNameForUid(int uid) -> String`: the uid's package, null when
            // no package owns it (`PackageManagerService.getNameForUid`).
            // `Context.checkCallingPermission` reaches it through the framework
            // to turn the calling uid into a package name.
            "getNameForUid" => {
                let uid = args.first().and_then(Value::as_int).unwrap_or(FIXTURE_UID);
                vec![Value::Str(state.name_for_uid(uid))]
            }
            "getPackageUid" => {
                let package = args.first().and_then(Value::as_str);
                match Self::lookup(&state, package) {
                    Ok(record) => vec![Value::Int(record.app.uid)],
                    Err(_) => vec![Value::Int(-1)],
                }
            }
            "hasSystemFeature" => {
                let name = args.first().and_then(Value::as_str).unwrap_or_default();
                vec![Value::Bool(state.has_system_feature(name))]
            }
            // `getSystemAvailableFeatures() : ParceledListSlice<FeatureInfo>`.
            // The answer is the declared feature table, so
            // `PackageManager.getSystemAvailableFeatures()` on this device
            // returns exactly what `hasSystemFeature` would confirm — no feature
            // is listed that is not also answered true, and a `false` entry
            // stays absent (the platform omits disabled features).
            "getSystemAvailableFeatures" => {
                // The webview feature is not a constant: it is present exactly
                // when the provider package is installed (ADR-0031).
                let webview = state
                    .has_webview_provider()
                    .then_some((crate::webview::WEBVIEW_FEATURE, true));
                let items = state
                    .features
                    .declared()
                    .into_iter()
                    .chain(webview)
                    .filter(|(_, enabled)| *enabled)
                    .map(|(name, _)| {
                        Value::Parcelable(Parcelable::ordered(
                            "android.content.pm.FeatureInfo",
                            vec![
                                Value::Str(Some(name.to_string())),
                                Value::Int(0), // version
                                Value::Int(0), // reqGlEsVersion
                                Value::Int(0), // flags
                            ],
                        ))
                    })
                    .collect();
                vec![Value::Parcelable(crate::parcelable::parceled_list_of(
                    items,
                ))]
            }
            "notifyDexLoad" => {
                // `Map<String,String>` after the package name: W26's
                // `codec::read_args_override` decodes the image's writer
                // (`writeInt(size)` + size x `writeString(key), writeString(value)`),
                // which the generated table had as `[I32, I32]`.
                let package = args.first().and_then(Value::as_str).unwrap_or_default();
                let dex_files = match args.get(1) {
                    Some(Value::Map(map)) => map.as_ref().map_or(0, Vec::len),
                    _ => 0,
                };
                let loader = args.get(2).and_then(Value::as_str).unwrap_or_default();
                state
                    .dex_loads
                    .push((package.to_string(), loader.to_string(), dex_files));
                Vec::new()
            }
            "checkPermission" => {
                // `checkPermission(String permName, String pkgName, int userId)`.
                // The answer is the *named* package's grant, not the package
                // under test's — through the one runtime table when the name is
                // the package that table describes (MODNOTIF2), and that
                // package's manifest grants otherwise.
                let permission = args.first().and_then(Value::as_str).unwrap_or_default();
                let package = args.get(1).and_then(Value::as_str);
                vec![Value::Int(state.check_permission_for(package, permission))]
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
                    Ok(record) => vec![Value::Str(record.installer_package.clone())],
                    _ => vec![Value::Str(None)],
                }
            }
            // W32: `IPackageManager.getComponentEnabledSetting` (code 85)
            // `(ComponentName, userId) -> int` and
            // `setComponentEnabledSetting` (code 83)
            // `(ComponentName, newState, flags, userId, callingPackage) -> void`.
            // The wire order is the image's `$Stub$Proxy`/`$Stub` order from the
            // generated table; `ComponentName.writeToParcel` is `package`,
            // `class`. Stock answers DEFAULT (0) until something sets a value.
            "getComponentEnabledSetting" => {
                let (package, class) = component_parts(args.first().and_then(Value::as_parcelable));
                let _user_id = args.get(1).and_then(Value::as_int).unwrap_or(0);
                vec![Value::Int(state.component_setting(&package, &class))]
            }
            "setComponentEnabledSetting" => {
                let (package, class) = component_parts(args.first().and_then(Value::as_parcelable));
                let new_state = args.get(1).and_then(Value::as_int).unwrap_or(-1);
                let _flags = args.get(2).and_then(Value::as_int).unwrap_or(0);
                let _user_id = args.get(3).and_then(Value::as_int).unwrap_or(0);
                let _calling_package = args.get(4).and_then(Value::as_str).unwrap_or_default();
                if !state.set_component_setting(&package, &class, new_state) {
                    // The framework's own reply to an invalid state is a
                    // `SecurityException`/`IllegalArgumentException`; there is
                    // no valid AIDL value, so nothing is stored and the
                    // request is reported as a bad request.
                    return Err(ServiceError::BadRequest {
                        interface: interface.to_string(),
                        method: method.name.clone(),
                        detail: format!(
                            "setComponentEnabledSetting: newState {new_state} is not a \
                             COMPONENT_ENABLED_STATE_* value"
                        ),
                    });
                }
                Vec::new()
            }
            // W32: `IPackageManager.queryProperty(String, userId) ->
            // ParceledListSlice<Property>`. `acos-apk` parses no `<property>`
            // manifest elements (API 31+ `android:property`), so the honest
            // stock answer is the empty result — the typed `null` slice the
            // framework's `queryProperty` caller turns into an empty list,
            // never a fabricated `Property`.
            "queryProperty" => vec![Value::TypedObject(Parcelable::Null)],
            "resolveContentProvider" => {
                // `resolveContentProvider(String authority, long flags, int userId)`.
                // This is the call `androidx.core.content.FileProvider` makes
                // with `GET_META_DATA` to read its `FILE_PROVIDER_PATHS` XML,
                // and what `ContentResolver` uses for a provider it has not
                // published yet; `PackageInfoUtils.generateProviderInfo` gates
                // the metadata `Bundle` on the same flag.
                let authority = args.first().and_then(Value::as_str).unwrap_or_default();
                let flags = flags_arg(args, 1);
                let found = state.provider_for_authority(authority);
                trace_query(
                    "resolveContentProvider",
                    authority,
                    flags,
                    found.as_ref().map_or(0, |(_, p)| p.meta_data.len()),
                );
                cache_provider_info(found.map(|(r, p)| (r.app.clone(), p.clone())));
                vec![match found {
                    Some((r, p)) => {
                        Self::cache(r);
                        Value::TypedObject(p.to_parcelable_with_flags(&r.app, flags))
                    }
                    None => Value::TypedObject(Parcelable::Null),
                }]
            }
            "queryContentProviders" => {
                // `queryContentProviders(String processName, int uid, long flags,
                // String metadataKey)`. The reply is a `ParceledListSlice`; the
                // real list is materialised by `pms_java` from the cache.
                cache_provider_list(
                    state
                        .packages
                        .iter()
                        .flat_map(|r| r.providers.iter().map(move |p| (r.app.clone(), p.clone())))
                        .collect(),
                );
                vec![Value::TypedObject(Parcelable::Null)]
            }
            "resolveIntent" | "queryIntentActivities" => {
                // `resolveIntent(Intent, String resolvedType, long flags, int
                // userId)` / `queryIntentActivities(Intent, String, long, int)`.
                // A session installs more than one package, so the intent is
                // matched against every package's declared filters (`ManifestDb`
                // is the same table ATMS resolves `startActivity` with) and the
                // matches are materialised by `pms_java`.
                let intent = args
                    .first()
                    .and_then(Value::as_parcelable)
                    .map(crate::ams::IntentFacts::from_parcelable)
                    .unwrap_or_default();
                let db = crate::ams::ManifestDb {
                    packages: state.packages.iter().map(|r| r.manifest.clone()).collect(),
                };
                let mut resolved: Vec<ResolvedActivity> = Vec::new();
                for declared in db.matching_activities(&intent) {
                    let Some(record) = state.lookup(Some(&declared.package)) else {
                        continue;
                    };
                    let Some(activity) = record
                        .package
                        .activities
                        .iter()
                        .find(|a| a.name == declared.class)
                        .cloned()
                    else {
                        continue;
                    };
                    resolved.push(ResolvedActivity {
                        app: record.app.clone(),
                        activity,
                        declared,
                    });
                }
                cache_intent_activities(resolved);
                // The generic reply is a placeholder: `pms_java` writes the real
                // `ResolveInfo`/`ParceledListSlice<ResolveInfo>`.
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
            // K2/R3: read-only package queries. The permission controller is
            // the AOSP module package (a device with the module has it); an
            // install-source info for a locally installed APK has a null
            // installer chain and package source, not an invented one.
            "getPermissionControllerPackageName" => {
                vec![Value::Str(Some("com.android.permissioncontroller".into()))]
            }
            "getInstallSourceInfo" => vec![Value::TypedObject(crate::value::Parcelable::ordered(
                "android.content.pm.InstallSourceInfo",
                vec![
                    Value::Str(None),
                    Value::NamedParcelable(crate::value::Parcelable::Null),
                    Value::Str(None),
                    Value::Str(None),
                    Value::Str8(None),
                    Value::Int(0),
                ],
            ))],
            "queryIntentContentProviders" => {
                vec![Value::TypedObject(parcelable::parceled_list_of(Vec::new()))]
            }
            "getPackageInstaller" => vec![Value::Binder(Some(self.wiring.package_installer))],
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

    /// `android.software.webview` is a package fact, not a device constant
    /// (ADR-0031): the feature is false with no provider package installed —
    /// which is what makes `WebViewFactory.getProvider()` throw the honest
    /// `UnsupportedOperationException` — and true once the session image has
    /// the provider package.
    #[test]
    fn webview_feature_follows_the_provider_package() {
        let mut state = PmsState::hello_views();
        assert!(
            !state.has_system_feature(crate::webview::WEBVIEW_FEATURE),
            "no provider package installed = no webview feature"
        );
        assert!(
            !state
                .features
                .declared()
                .iter()
                .any(|(n, _)| *n == crate::webview::WEBVIEW_FEATURE),
            "the static table must not claim the feature"
        );

        let mut record = state.packages[state.primary].clone();
        record.package.package_name = crate::webview::PROVIDER_PACKAGE.into();
        record.app.package_name = crate::webview::PROVIDER_PACKAGE.into();
        record.package.application = record.app.clone();
        state.packages.push(record);
        assert!(
            state.has_system_feature(crate::webview::WEBVIEW_FEATURE),
            "the installed provider package is the feature"
        );
    }

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
            permissions: Vec::new(),
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
                // W31 added the rest of `parseBaseApplication`'s flag inputs;
                // absent attributes stay `None` so the stock defaults apply.
                ..Default::default()
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
            meta_data: Vec::new(),
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
            meta_data: Vec::new(),
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
        let decoded = ServiceInfo::from_parcelable(&service.to_parcelable(&app)).expect("decoded");
        assert_eq!(decoded, service);
    }

    #[test]
    fn resolve_content_provider_answers_the_declared_authority() {
        let mut state = PmsState::new();
        assert!(state
            .provider_for_authority("dev.acos.fixtures.svcprovider")
            .is_none());
        state.primary_mut().providers.push(sample_provider());
        let found = state
            .provider_for_authority("dev.acos.fixtures.svcprovider")
            .expect("declared authority resolves");
        assert_eq!(found.1.name, sample_provider().name);
        assert!(state
            .provider_for_authority("dev.acos.fixtures.absent")
            .is_none());
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
            .expect("fixture authority declared in the manifest")
            .1;
        assert_eq!(provider.name, "dev.acos.fixtures.svcprovider.EchoProvider");
        assert!(!provider.exported, "the fixture provider is not exported");
        let service = state
            .primary()
            .services
            .iter()
            .find(|s| s.name == "dev.acos.fixtures.svcprovider.EchoService")
            .expect("fixture service declared in the manifest");
        assert!(!service.exported, "the fixture service is not exported");
    }

    /// `PackageParser` keeps a literal `android:label` in `nonLocalizedLabel`
    /// (and leaves `labelRes` 0), and a resource label in `labelRes` with no
    /// literal. `ActivityInfo.loadLabel` walks activity -> application ->
    /// component name, so an activity without its own label must be "unset"
    /// (not a copy of the application label, and never the class name).
    #[test]
    fn label_specs_follow_package_parser() {
        let literal = "Literal Name".to_string();
        assert_eq!(
            LabelSpec::from_parts(Some(&literal), Some(0x7f0f0001)),
            LabelSpec {
                res: 0,
                literal: Some(literal.clone()),
            },
            "a literal label wins and clears labelRes"
        );
        assert_eq!(
            LabelSpec::from_parts(None, Some(0x7f0f0001)),
            LabelSpec {
                res: 0x7f0f0001,
                literal: None,
            }
        );
        let unset = LabelSpec::from_parts(None, None);
        assert!(
            unset.is_unset(),
            "no label attribute leaves both fields unset"
        );
        assert!(!LabelSpec::from_parts(None, Some(1)).is_unset());

        let labels = ManifestLabels {
            application: LabelSpec {
                res: 0x7f0f0002,
                literal: None,
            },
            activities: vec![
                (
                    "pkg.WithLabel".into(),
                    LabelSpec::from_parts(Some(&literal), None),
                ),
                ("pkg.NoLabel".into(), LabelSpec::default()),
            ],
        };
        assert_eq!(
            labels.activity("pkg.WithLabel").literal.as_deref(),
            Some("Literal Name")
        );
        assert!(labels.activity("pkg.NoLabel").is_unset());
        assert!(
            labels.activity("pkg.NotDeclared").is_unset(),
            "an unknown activity must not inherit another component's label"
        );
    }
}
