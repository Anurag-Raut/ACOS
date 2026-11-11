//! Parcelable codecs.
//!
//! The wire layout of a Java `Parcelable` is its own `writeToParcel` bytecode.
//! ACOS does not guess it: [`FieldOp`] tables are transcribed from the
//! mechanically-scanned `writeToParcel` of the Android 14 boot DEX (see
//! [`scan`] and `examples/scan_parcelables.rs`). The scanner is also a test
//! oracle: [`verify_prefix`] asserts that a table's op kinds are a prefix of the
//! real method, so drift is caught.
//!
//! ## Verification status
//!
//! `FULL` means every `Parcel.write*` in the real method is represented.
//! `PREFIX` means the leading fields (the ones ACOS reads/writes) match the real
//! order and the remainder is documented as a gap. `CURATED` means the type is
//! a small ACOS-owned value whose Java reader is not on the V1 path.

use std::sync::OnceLock;

use acos_parcel::{Parcel, ParcelError};
use acos_protocol::ErrorCode;

use crate::error::{ServiceError, Result};
use crate::value::{Bundle, Parcelable, RawFdValue, Value};

/// One field in `writeToParcel` order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldOp {
    /// `writeInt`.
    Int,
    /// `writeLong`.
    Long,
    /// `writeFloat`.
    Float,
    /// `writeDouble`.
    Double,
    /// `writeBoolean`.
    Bool,
    /// `writeByte`.
    Byte,
    /// `writeChar`.
    Char,
    /// `writeString` (UTF-16).
    Str16,
    /// `TextUtils.writeToParcel`: an `int` kind marker followed by a
    /// `writeString8` (the non-spanned branch, `kind = 1`). ACOS only ever
    /// writes the unspanned/null form.
    CharSequence,
    /// `writeString8`.
    Str8,
    /// `writeStringArray`.
    StringArray,
    /// `writeIntArray`.
    IntArray,
    /// `writeLongArray`.
    LongArray,
    /// `writeFloatArray`.
    FloatArray,
    /// `writeByteArray`.
    ByteArray,
    /// `writeSparseArray`. ACOS only models the `null` encoding (`-1`); a
    /// non-null `SparseArray` is a structured `UnsupportedParcelable`.
    SparseArray,
    /// `writeParcelable` (class name + payload).
    Named(&'static str),
    /// `writeTypedObject` (presence marker + payload).
    Typed(&'static str),
    /// `writeBundle`.
    Bundle,
    /// A raw file descriptor (`writeFileDescriptor`).
    Fd,
    /// A strong binder (`writeStrongBinder`) with no presence marker.
    Binder,
    /// `writeTypedArray` (an int count then each element's payload).
    TypedArray(&'static str),
    /// A nested `Parcelable` written as a bare payload (no class name and no
    /// presence marker). The API-34 `Configuration.writeToParcel` calls
    /// `windowConfiguration.writeToParcel(dest, flags)` directly, and
    /// `WindowConfiguration.writeToParcel` calls `mBounds.writeToParcel` and
    /// `mMaxBounds.writeToParcel` the same way; a `writeParcelable` /
    /// `writeTypedObject` op would add the wrong marker/class name.
    Nested(&'static str),
    /// A `writeParcelableList` argument: an int count then each element's
    /// class name + payload (`ClientTransaction.mCallbacks`).
    ParcelableList(&'static str),
    /// A field ACOS does not model; reads/writes a `null` of the given kind.
    IgnoredNamed(&'static str),
    /// A field ACOS does not model; reads/writes a `false` presence marker.
    IgnoredTyped(&'static str),
    /// A dynamic `writeValue`.
    Dynamic,
    /// A conditional group: a `writeInt` presence marker, then the nested
    /// fields when the marker is non-zero (API-34 `TaskDescription` writes its
    /// label, icon and icon filename this way). The nested list must be empty
    /// for an absent field.
    Cond(&'static [FieldOp]),
    /// `Parcel.writePersistableBundle`: the same wire shape as [`FieldOp::Bundle`]
    /// (an int length, `-1` for null, then the `BaseBundle` map).
    PersistableBundle,
    /// `Parcel.writeTypedList`: an int count (`-1` for null) then each element's
    /// `writeTypedObject` presence marker + payload.
    TypedList(&'static str),
    /// `Parcel.writeStrongInterface`: a strong binder (`null` is allowed).
    Interface,
    /// A `writeInt` count followed by that many `writeTypedObject` elements
    /// (the API-34 `InsetsState.mSources` loop).
    CountedTyped(&'static str),
    /// A payload ACOS cannot model. Reading or writing it raises a structured
    /// `UnsupportedParcelable` rather than silently desynchronising the parcel.
    Unsupported(&'static str),
}

/// How faithfully a spec covers the real method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verification {
    /// Every write call is represented.
    Full,
    /// The leading fields are represented in the real order.
    Prefix,
    /// An ACOS-owned value whose Java reader is not on the V1 path.
    Curated,
}

/// A parcelable wire spec.
#[derive(Debug)]
pub struct ParcelableSpec {
    /// Dotted class name.
    pub class: &'static str,
    /// Fields in `writeToParcel` order.
    pub fields: &'static [FieldOp],
    /// Coverage of the real method.
    pub verification: Verification,
}

/// Every codec ACOS ships.
pub static SPECS: &[ParcelableSpec] = &[
    ParcelableSpec {
        class: "android.graphics.Rect",
        fields: &[FieldOp::Int, FieldOp::Int, FieldOp::Int, FieldOp::Int],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.graphics.Point",
        fields: &[FieldOp::Int, FieldOp::Int],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.content.ComponentName",
        fields: &[FieldOp::Str16, FieldOp::Str16],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.content.pm.PackageItemInfo",
        fields: &[
            FieldOp::Str8,  // name
            FieldOp::Str8,  // packageName
            FieldOp::Int,   // banner
            FieldOp::CharSequence, // label (TextUtils.writeToParcel)
            FieldOp::Int,   // icon
            FieldOp::Int,   // labelRes
            FieldOp::Bundle,
            FieldOp::Int, // showUser
            FieldOp::Int, // showBadge (boolean on the wire)
        ],
        verification: Verification::Full,
    },
    // ApplicationInfo: the full API-34 `writeToParcel` (82 `Parcel.write*`
    // calls after `PackageItemInfo` is flattened). `maybeWriteSquashed` always
    // emits its `writeInt` marker when squashing is disabled, which is the case
    // for a directly-called `writeToParcel`. The `Parcelling` helpers
    // (`ForBoolean`, `ForStringSet`) are not `Parcel` methods and are invisible
    // to the scanner; ACOS keeps them as documented gaps in the byte stream.
    ParcelableSpec {
        class: "android.content.pm.ApplicationInfo",
        fields: &[
            FieldOp::Int,  // maybeWriteSquashed marker (0 = not squashed)
            FieldOp::Str8, // PackageItemInfo.name
            FieldOp::Str8, // PackageItemInfo.packageName
            FieldOp::Int,  // banner
            FieldOp::CharSequence, // label (TextUtils.writeToParcel)
            FieldOp::Int,  // icon
            FieldOp::Int,  // labelRes
            FieldOp::Bundle,
            FieldOp::Int,  // showUser
            FieldOp::Int,  // showBadge (isArchived)
            FieldOp::Str8, // taskAffinity
            FieldOp::Str8, // permission
            FieldOp::Str8, // processName
            FieldOp::Str8, // className
            FieldOp::Int,  // theme
            FieldOp::Int,  // flags
            FieldOp::Int,  // privateFlags
            FieldOp::Int,  // privateFlagsExt
            FieldOp::Int,  // requiresSmallestWidthDp
            FieldOp::Int,  // compatibleWidthLimitDp
            FieldOp::Int,  // largestWidthLimitDp
            FieldOp::Cond(&[FieldOp::Long, FieldOp::Long]), // storageUuid
            FieldOp::Str8, // scanSourceDir
            FieldOp::Str8, // scanPublicSourceDir
            FieldOp::Str8, // sourceDir
            FieldOp::Str8, // publicSourceDir
            FieldOp::StringArray, // splitNames
            FieldOp::StringArray, // splitSourceDirs
            FieldOp::StringArray, // splitPublicSourceDirs
            FieldOp::SparseArray, // splitDependencies
            FieldOp::Str8, // nativeLibraryDir
            FieldOp::Str8, // secondaryNativeLibraryDir
            FieldOp::Str8, // nativeLibraryRootDir
            FieldOp::Int,  // nativeLibraryRootRequiresIsa
            FieldOp::Str8, // primaryCpuAbi
            FieldOp::Str8, // secondaryCpuAbi
            FieldOp::StringArray, // resourceDirs
            FieldOp::StringArray, // overlayPaths
            FieldOp::Str8, // seInfo
            FieldOp::Str8, // seInfoUser
            FieldOp::StringArray, // sharedLibraryFiles
            FieldOp::TypedList("android.content.pm.SharedLibraryInfo"), // sharedLibraryInfos
            FieldOp::Str8, // dataDir
            FieldOp::Str8, // deviceProtectedDataDir
            FieldOp::Str8, // credentialProtectedDataDir
            FieldOp::Int,  // uid
            FieldOp::Int,  // minSdkVersion
            FieldOp::Int,  // targetSdkVersion
            FieldOp::Long, // longVersionCode
            FieldOp::Int,  // enabled
            FieldOp::Int,  // enabledSetting
            FieldOp::Int,  // installLocation
            FieldOp::Str8, // manageSpaceActivityName
            FieldOp::Str8, // backupAgentName
            FieldOp::Int,  // descriptionRes
            FieldOp::Int,  // uiOptions
            FieldOp::Int,  // fullBackupContent
            FieldOp::Int,  // dataExtractionRulesRes
            FieldOp::Bool, // crossProfile
            FieldOp::Int,  // networkSecurityConfigRes
            FieldOp::Int,  // category
            FieldOp::Int,  // targetSandboxVersion
            FieldOp::Str8, // classLoaderName
            FieldOp::StringArray, // splitClassLoaderNames
            FieldOp::Int,  // compileSdkVersion
            FieldOp::Str8, // compileSdkVersionCodename
            FieldOp::Str8, // appComponentFactory
            FieldOp::Int,  // iconRes
            FieldOp::Int,  // roundIconRes
            FieldOp::Int,  // mHiddenApiPolicy
            FieldOp::Int,  // hiddenUntilInstalled
            FieldOp::Str8, // zygotePreloadName
            FieldOp::Int,  // gwpAsanMode
            FieldOp::Int,  // memtagMode
            FieldOp::Int,  // nativeHeapZeroInitialized
            FieldOp::Int,  // requestRawExternalStorageAccess (ForBoolean)
            FieldOp::Long, // createTimestamp
            FieldOp::Cond(&[FieldOp::Str16, FieldOp::Str16]), // mAppClassNamesByProcess
            FieldOp::Int,  // localeConfigRes
            FieldOp::Int,  // mKnownActivityEmbeddingCerts (ForStringSet)
        ],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.content.pm.ComponentInfo",
        fields: &[
            FieldOp::Str8, // PackageItemInfo.name
            FieldOp::Str8, // PackageItemInfo.packageName
            FieldOp::Int,  // banner
            FieldOp::CharSequence, // label
            FieldOp::Int,  // icon
            FieldOp::Int,  // labelRes
            FieldOp::Bundle,
            FieldOp::Int,  // showUser
            FieldOp::Int,  // showBadge
            FieldOp::Nested("android.content.pm.ApplicationInfo"),
            FieldOp::Str8,        // processName
            FieldOp::Str8,        // splitName
            FieldOp::StringArray, // attributionTags
            FieldOp::Int,         // descriptionRes
            FieldOp::Int,         // enabled
            FieldOp::Int,         // exported
            FieldOp::Int,         // directBootAware
        ],
        verification: Verification::Full,
    },
    // `android.content.pm.ActivityInfo.writeToParcel` (API 34): the
    // `ComponentInfo` prefix, then the `ActivityInfo` fields including the
    // conditional `windowLayout`. The `sForStringSet.parcel` helper is invisible
    // to a `Parcel.write*` scanner (it is not a `Parcel` method).
    ParcelableSpec {
        class: "android.content.pm.ActivityInfo",
        fields: &[
            // ComponentInfo prefix
            FieldOp::Str8,
            FieldOp::Str8,
            FieldOp::Int,
            FieldOp::CharSequence,
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Bundle,
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Nested("android.content.pm.ApplicationInfo"),
            FieldOp::Str8,
            FieldOp::Str8,
            FieldOp::StringArray,
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Int,
            // ActivityInfo
            FieldOp::Int,  // theme
            FieldOp::Int,  // launchMode
            FieldOp::Int,  // documentLaunchMode
            FieldOp::Str8, // permission
            FieldOp::Str8, // taskAffinity
            FieldOp::Str8, // targetActivity
            FieldOp::Str8, // launchToken
            FieldOp::Int,  // flags
            FieldOp::Int,  // privateFlags
            FieldOp::Int,  // screenOrientation
            FieldOp::Int,  // configChanges
            FieldOp::Int,  // softInputMode
            FieldOp::Int,  // uiOptions
            FieldOp::Str8, // parentActivityName
            FieldOp::Int,  // persistableMode
            FieldOp::Int,  // maxRecents
            FieldOp::Int,  // lockTaskLaunchMode
            FieldOp::Cond(&[FieldOp::Nested("android.content.pm.ActivityInfo$WindowLayout")]),
            FieldOp::Int,  // resizeMode
            FieldOp::Str8, // requestedVrComponent
            FieldOp::Int,  // rotationAnimation
            FieldOp::Int,  // colorMode
            FieldOp::Float, // mMaxAspectRatio
            FieldOp::Float, // mMinAspectRatio
            FieldOp::Bool, // supportsSizeChanges
            FieldOp::Int,  // mKnownActivityEmbeddingCerts (ForStringSet)
            FieldOp::Str8, // requiredDisplayCategory
        ],
        verification: Verification::Full,
    },
    // `PackageInfo.writeToParcel` (API 34): 43 `Parcel.write*` calls. The two
    // conditional members (`applicationInfo`, `signingInfo`) are `writeInt`
    // presence markers followed by a direct `writeToParcel`.
    ParcelableSpec {
        class: "android.content.pm.PackageInfo",
        fields: &[
            FieldOp::Str8,        // packageName
            FieldOp::StringArray, // splitNames
            FieldOp::Int,         // versionCode
            FieldOp::Int,         // versionCodeMajor
            FieldOp::Str8,        // versionName
            FieldOp::Int,         // baseRevisionCode
            FieldOp::IntArray,    // splitRevisionCodes
            FieldOp::Str8,        // sharedUserId
            FieldOp::Int,         // sharedUserLabel
            FieldOp::Cond(&[FieldOp::Nested("android.content.pm.ApplicationInfo")]),
            FieldOp::Long,        // firstInstallTime
            FieldOp::Long,        // lastUpdateTime
            FieldOp::IntArray,    // gids
            FieldOp::TypedArray("android.content.pm.ActivityInfo"),
            FieldOp::TypedArray("android.content.pm.ActivityInfo"), // receivers
            FieldOp::TypedArray("android.content.pm.ServiceInfo"),
            FieldOp::TypedArray("android.content.pm.ProviderInfo"),
            FieldOp::TypedArray("android.content.pm.InstrumentationInfo"),
            FieldOp::TypedArray("android.content.pm.PermissionInfo"),
            FieldOp::StringArray, // requestedPermissions
            FieldOp::IntArray,    // requestedPermissionsFlags
            FieldOp::TypedArray("android.content.pm.Signature"),
            FieldOp::TypedArray("android.content.pm.ConfigurationInfo"),
            FieldOp::TypedArray("android.content.pm.FeatureInfo"),
            FieldOp::TypedArray("android.content.pm.FeatureGroupInfo"),
            FieldOp::TypedArray("android.content.pm.Attribution"),
            FieldOp::Int,         // installLocation
            FieldOp::Int,         // isStub
            FieldOp::Int,         // coreApp
            FieldOp::Int,         // requiredForAllUsers
            FieldOp::Str8,        // restrictedAccountType
            FieldOp::Str8,        // requiredAccountType
            FieldOp::Str8,        // overlayTarget
            FieldOp::Str8,        // overlayCategory
            FieldOp::Int,         // overlayPriority
            FieldOp::Bool,        // mOverlayIsStatic
            FieldOp::Int,         // compileSdkVersion
            FieldOp::Str8,        // compileSdkVersionCodename
            FieldOp::Cond(&[FieldOp::Nested("android.content.pm.SigningInfo")]),
            FieldOp::Bool,        // isApex
            FieldOp::Bool,        // isActiveApex
        ],
        verification: Verification::Full,
    },
    // `ProviderInfo.writeToParcel` (API 34): the `ComponentInfo` prefix then
    // the provider fields (28 `Parcel.write*` calls).
    ParcelableSpec {
        class: "android.content.pm.ProviderInfo",
        fields: &[
            FieldOp::Str8,        // PackageItemInfo.name
            FieldOp::Str8,        // packageName
            FieldOp::Int,         // banner
            FieldOp::CharSequence, // label
            FieldOp::Int,         // icon
            FieldOp::Int,         // labelRes
            FieldOp::Bundle,      // metaData
            FieldOp::Int,         // showUser
            FieldOp::Int,         // showBadge
            FieldOp::Nested("android.content.pm.ApplicationInfo"),
            FieldOp::Str8,        // processName
            FieldOp::Str8,        // splitName
            FieldOp::StringArray, // attributionTags
            FieldOp::Int,         // descriptionRes
            FieldOp::Int,         // enabled
            FieldOp::Int,         // exported
            FieldOp::Int,         // directBootAware
            FieldOp::Str8,        // authority
            FieldOp::Str8,        // readPermission
            FieldOp::Str8,        // writePermission
            FieldOp::Int,         // grantUriPermissions
            FieldOp::Int,         // forceUriPermissions
            FieldOp::TypedArray("android.content.pm.PathPermission"),
            FieldOp::TypedArray("android.content.pm.PathPermission"),
            FieldOp::Int,         // multiprocess
            FieldOp::Int,         // initOrder
            FieldOp::Int,         // flags
            FieldOp::Int,         // isSyncable
        ],
        verification: Verification::Full,
    },
    // `ServiceInfo.writeToParcel` (API 34): the `ComponentInfo` prefix splice
    // then `permission`, `flags` and `mForegroundServiceType`. The DEX confirms
    // the `flags` write is second (the old prefix codec guessed `exported`).
    ParcelableSpec {
        class: "android.content.pm.ServiceInfo",
        fields: &[
            // ComponentInfo prefix (super.writeToParcel splice)
            FieldOp::Str8,        // PackageItemInfo.name
            FieldOp::Str8,        // packageName
            FieldOp::Int,         // banner
            FieldOp::CharSequence, // label
            FieldOp::Int,         // icon
            FieldOp::Int,         // labelRes
            FieldOp::Bundle,      // metaData
            FieldOp::Int,         // showUser
            FieldOp::Int,         // showBadge
            FieldOp::Nested("android.content.pm.ApplicationInfo"),
            FieldOp::Str8,        // processName
            FieldOp::Str8,        // splitName
            FieldOp::StringArray, // attributionTags
            FieldOp::Int,         // descriptionRes
            FieldOp::Int,         // enabled
            FieldOp::Int,         // exported (ComponentInfo)
            FieldOp::Int,         // directBootAware
            // ServiceInfo
            FieldOp::Str8, // permission
            FieldOp::Int,  // flags
            FieldOp::Int,  // mForegroundServiceType
        ],
        verification: Verification::Full,
    },
    // `InstrumentationInfo.writeToParcel` (API 34): the `PackageItemInfo`
    // prefix then 17 fields. `splitDependencies` is a `SparseArray` (ACOS
    // models only its `null` encoding; a non-null array is structured
    // `UnsupportedParcelable`).
    ParcelableSpec {
        class: "android.content.pm.InstrumentationInfo",
        fields: &[
            // PackageItemInfo prefix (super.writeToParcel splice)
            FieldOp::Str8,        // PackageItemInfo.name
            FieldOp::Str8,        // packageName
            FieldOp::Int,         // banner
            FieldOp::CharSequence, // label
            FieldOp::Int,         // icon
            FieldOp::Int,         // labelRes
            FieldOp::Bundle,      // metaData
            FieldOp::Int,         // showUser
            FieldOp::Int,         // showBadge
            // InstrumentationInfo
            FieldOp::Str8,        // targetPackage
            FieldOp::Str8,        // targetProcesses
            FieldOp::Str8,        // sourceDir
            FieldOp::Str8,        // publicSourceDir
            FieldOp::StringArray, // splitNames
            FieldOp::StringArray, // splitSourceDirs
            FieldOp::StringArray, // splitPublicSourceDirs
            FieldOp::SparseArray, // splitDependencies
            FieldOp::Str8,        // dataDir
            FieldOp::Str8,        // deviceProtectedDataDir
            FieldOp::Str8,        // credentialProtectedDataDir
            FieldOp::Str8,        // primaryCpuAbi
            FieldOp::Str8,        // secondaryCpuAbi
            FieldOp::Str8,        // nativeLibraryDir
            FieldOp::Str8,        // secondaryNativeLibraryDir
            FieldOp::Int,         // handleProfiling
            FieldOp::Int,         // functionalTest
        ],
        verification: Verification::Full,
    },
    // `PermissionInfo.writeToParcel` (API 34): the `PackageItemInfo` prefix
    // then 8 fields. `knownCerts` is a `Parcelling.ForStringSet`; the scanner
    // only sees its leading `writeInt` (a null set writes `-1`), so the spec
    // models the null form and a non-null set is a documented gap.
    ParcelableSpec {
        class: "android.content.pm.PermissionInfo",
        fields: &[
            // PackageItemInfo prefix (super.writeToParcel splice)
            FieldOp::Str8,        // PackageItemInfo.name
            FieldOp::Str8,        // packageName
            FieldOp::Int,         // banner
            FieldOp::CharSequence, // label
            FieldOp::Int,         // icon
            FieldOp::Int,         // labelRes
            FieldOp::Bundle,      // metaData
            FieldOp::Int,         // showUser
            FieldOp::Int,         // showBadge
            // PermissionInfo
            FieldOp::Int,          // protectionLevel
            FieldOp::Int,          // flags
            FieldOp::Str8,         // group
            FieldOp::Str8,         // backgroundPermission
            FieldOp::Int,          // descriptionRes
            FieldOp::Int,          // requestRes
            FieldOp::CharSequence, // nonLocalizedDescription
            FieldOp::Int,          // knownCerts (ForStringSet leading int)
        ],
        verification: Verification::Full,
    },
    // `ResolveInfo.writeToParcel` (API 34): a component-type marker selects one
    // of `ActivityInfo`/`ServiceInfo`/`ProviderInfo` (1/2/3), then an optional
    // `IntentFilter`, then the scalar match metadata. ACOS models the launcher
    // `ActivityInfo` branch (`Cond` writes the marker `1`); the `ServiceInfo`
    // (`2`) and `ProviderInfo` (`3`) branches share the same marker slot and are
    // not expressible with `Cond`. `IntentFilter` has no codec yet, so a present
    // filter is a structured `UnsupportedParcelable`.
    ParcelableSpec {
        class: "android.content.pm.ResolveInfo",
        fields: &[
            FieldOp::Cond(&[FieldOp::Nested("android.content.pm.ActivityInfo")]),
            FieldOp::Cond(&[FieldOp::Nested("android.content.IntentFilter")]),
            FieldOp::Int,          // priority
            FieldOp::Int,          // preferredOrder
            FieldOp::Int,          // match
            FieldOp::Int,          // specificIndex
            FieldOp::Int,          // labelRes
            FieldOp::CharSequence, // nonLocalizedLabel
            FieldOp::Int,          // icon
            FieldOp::Str8,         // resolvePackageName
            FieldOp::Int,          // targetUserId
            FieldOp::Int,          // system
            FieldOp::Int,          // noResourceId
            FieldOp::Int,          // iconResourceId
            FieldOp::Int,          // handleAllWebDataURI
            FieldOp::Int,          // mAutoResolutionAllowed
            FieldOp::Int,          // isInstantAppAvailable
            FieldOp::Int,          // userHandle identifier
        ],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.app.ContentProviderHolder",
        fields: &[
            FieldOp::Typed("android.content.pm.ProviderInfo"),
            FieldOp::Typed("android.os.IBinder"),
            FieldOp::Bool, // delay
        ],
        verification: Verification::Curated,
    },
    // DisplayInfo: the scalar leading order is transcribed from
    // `classes4.dex:Landroid/view/DisplayInfo;->writeToParcel`. The nested
    // `DisplayCutout`, `DisplayShape`, supported `Mode[]` and the trailing
    // `SparseArray` are modelled with ACOS-owned placeholders (gap list).
    ParcelableSpec {
        class: "android.view.DisplayInfo",
        fields: &[
            FieldOp::Int,              // displayId
            FieldOp::Int,              // logicalWidth
            FieldOp::Int,              // logicalHeight
            FieldOp::Int,              // appWidth
            FieldOp::Int,              // appHeight
            FieldOp::IgnoredNamed("android.view.DisplayCutout"),
            FieldOp::IgnoredNamed("android.view.DisplayShape"),
            FieldOp::Str8,             // name
            FieldOp::Int,              // flags
            FieldOp::Int,              // type
            FieldOp::Int,              // address
            FieldOp::Int,              // deviceProductInfo
            FieldOp::Int,              // rotation
            FieldOp::Int,              // modeId
            FieldOp::Int,              // renderFrameRate
            FieldOp::Int,              // supportedModes marker
            FieldOp::Int,              // logicalDensityDpi
            FieldOp::Int,              // state
            FieldOp::Float,            // refreshRate
            FieldOp::Int,              // colorMode
            FieldOp::Int,              // installOrientation
        ],
        verification: Verification::Prefix,
    },
    // `android.os.LocaleList.writeToParcel` is a single `writeString8`
    // (`mStringRepresentation`); `Configuration` wraps it in
    // `writeTypedObject`.
    ParcelableSpec {
        class: "android.os.LocaleList",
        fields: &[FieldOp::Str8],
        verification: Verification::Full,
    },
    // `android.app.WindowConfiguration.writeToParcel` (API 34):
    //   mBounds.writeToParcel(dest, flags)
    //   writeTypedObject(mAppBounds, flags)
    //   mMaxBounds.writeToParcel(dest, flags)
    //   writeInt(windowingMode/activityType/alwaysOnTop/rotation/
    //            displayWindowingMode/displayRotation)
    // The two `Rect` fields are direct `writeToParcel` calls, so they are
    // modelled as [`FieldOp::Nested`].
    ParcelableSpec {
        class: "android.app.WindowConfiguration",
        fields: &[
            FieldOp::Nested("android.graphics.Rect"), // mBounds
            FieldOp::Typed("android.graphics.Rect"),  // mAppBounds
            FieldOp::Nested("android.graphics.Rect"), // mMaxBounds
            FieldOp::Int, // windowingMode
            FieldOp::Int, // activityType
            FieldOp::Int, // alwaysOnTop
            FieldOp::Int, // rotation
            FieldOp::Int, // displayWindowingMode
            FieldOp::Int, // displayRotation
        ],
        verification: Verification::Full,
    },
    // `android.content.res.Configuration.writeToParcel` (API 34), transcribed
    // from `classes.dex:Landroid/content/res/Configuration;->writeToParcel`:
    //   writeFloat(fontScale)
    //   writeInt(mcc); writeInt(mnc)
    //   fixUpLocaleList(); writeTypedObject(mLocaleList)
    //   if (userSetLocale) writeInt(1) else writeInt(0)   [one op at runtime,
    //     two static `writeInt` branches; the scanner sees both]
    //   touchscreen, keyboard, keyboardHidden, hardKeyboardHidden,
    //   navigation, navigationHidden, orientation, screenLayout, colorMode,
    //   uiMode, screenWidthDp, screenHeightDp, smallestScreenWidthDp,
    //   densityDpi, compatScreenWidthDp, compatScreenHeightDp,
    //   compatSmallestScreenWidthDp
    //   windowConfiguration.writeToParcel(dest, flags)
    //   assetsSeq, seq, fontWeightAdjustment, mGrammaticalGender
    ParcelableSpec {
        class: "android.content.res.Configuration",
        fields: &[
            FieldOp::Float,                            // fontScale
            FieldOp::Int,                              // mcc
            FieldOp::Int,                              // mnc
            FieldOp::Typed("android.os.LocaleList"),    // mLocaleList
            FieldOp::Int,                              // userSetLocale
            FieldOp::Int,                              // touchscreen
            FieldOp::Int,                              // keyboard
            FieldOp::Int,                              // keyboardHidden
            FieldOp::Int,                              // hardKeyboardHidden
            FieldOp::Int,                              // navigation
            FieldOp::Int,                              // navigationHidden
            FieldOp::Int,                              // orientation
            FieldOp::Int,                              // screenLayout
            FieldOp::Int,                              // colorMode
            FieldOp::Int,                              // uiMode
            FieldOp::Int,                              // screenWidthDp
            FieldOp::Int,                              // screenHeightDp
            FieldOp::Int,                              // smallestScreenWidthDp
            FieldOp::Int,                              // densityDpi
            FieldOp::Int,                              // compatScreenWidthDp
            FieldOp::Int,                              // compatScreenHeightDp
            FieldOp::Int,                              // compatSmallestScreenWidthDp
            FieldOp::Nested("android.app.WindowConfiguration"), // windowConfiguration
            FieldOp::Int,                              // assetsSeq
            FieldOp::Int,                              // seq
            FieldOp::Int,                              // fontWeightAdjustment
            FieldOp::Int,                              // mGrammaticalGender
        ],
        verification: Verification::Full,
    },
    // Transcribed from API-34
    // `classes.dex:Landroid/app/ActivityManager$TaskDescription;->writeToParcel`:
    //   int labelPresent; if (labelPresent) writeString(label)
    //   int iconPresent;  if (iconPresent)  Icon.writeToParcel
    //   int colorPrimary; int colorBackground; int statusBarColor; int navBarColor
    //   boolean ensureStatusBarContrast; boolean ensureNavBarContrast
    //   int resizeMode; int minWidth; int minHeight
    //   int iconFilenamePresent; if (iconFilenamePresent) writeString(iconFilename)
    //   int colorBackgroundFloating
    // The three presence branches are why this spec uses [`FieldOp::Cond`]; the
    // old linear six-field layout read the icon-presence marker as a string
    // length and rejected the parcel (W60-A blocker).
    ParcelableSpec {
        class: "android.app.ActivityManager$TaskDescription",
        fields: &[
            FieldOp::Cond(&[FieldOp::Str16]), // label
            FieldOp::Cond(&[FieldOp::Unsupported("android.graphics.drawable.Icon")]), // icon
            FieldOp::Int,  // colorPrimary
            FieldOp::Int,  // colorBackground
            FieldOp::Int,  // statusBarColor
            FieldOp::Int,  // navigationBarColor
            FieldOp::Bool, // ensureStatusBarContrastWhenTransparent
            FieldOp::Bool, // ensureNavigationBarContrastWhenTransparent
            FieldOp::Int,  // resizeMode
            FieldOp::Int,  // minWidth
            FieldOp::Int,  // minHeight
            FieldOp::Cond(&[FieldOp::Str16]), // iconFilename
            FieldOp::Int,  // colorBackgroundFloating
        ],
        verification: Verification::Curated,
    },
    // `android.window.SizeConfigurationBuckets.writeToParcel`:
    //   writeByte(the single `mConfigurationChanged`/`mChanged` byte)
    //   writeIntArray(mHorizontalDps); writeIntArray(mVerticalDps);
    //   writeIntArray(mScreenWidthDps); writeIntArray(mScreenHeightDps)
    ParcelableSpec {
        class: "android.window.SizeConfigurationBuckets",
        fields: &[
            FieldOp::Byte,
            FieldOp::IntArray,
            FieldOp::IntArray,
            FieldOp::IntArray,
            FieldOp::IntArray,
        ],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.view.autofill.AutofillId",
        fields: &[FieldOp::Int, FieldOp::Int],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.view.autofill.AutofillValue",
        fields: &[FieldOp::Int, FieldOp::Dynamic],
        verification: Verification::Curated,
    },
    // The real API-34 `InputDevice.writeToParcel` starts with the nested
    // `KeyCharacterMap`, then the scalar fields. `mMotionRanges` is written as
    // a trailing count + 7-field records; ACOS's synthetic devices carry zero
    // ranges, so only the (zero) count is modelled. `mHostUsiVersion` is a
    // nested two-int parcelable.
    ParcelableSpec {
        class: "android.view.InputDevice",
        fields: &[
            FieldOp::Nested("android.view.KeyCharacterMap"), // mKeyCharacterMap
            FieldOp::Int,    // mId
            FieldOp::Int,    // mGeneration
            FieldOp::Int,    // mControllerNumber
            FieldOp::Str16,  // mName
            FieldOp::Int,    // mVendorId
            FieldOp::Int,    // mProductId
            FieldOp::Str16,  // mDescriptor
            FieldOp::Int,    // mIsExternal
            FieldOp::Int,    // mSources
            FieldOp::Int,    // mKeyboardType
            FieldOp::Str8,   // mKeyboardLanguageTag
            FieldOp::Str8,   // mKeyboardLayoutType
            FieldOp::Int,    // mHasVibrator
            FieldOp::Int,    // mHasMicrophone
            FieldOp::Int,    // mHasButtonUnderPad
            FieldOp::Int,    // mHasSensor
            FieldOp::Int,    // mHasBattery
            FieldOp::Nested("android.hardware.input.HostUsiVersion"),
            FieldOp::Int,    // mAssociatedDisplayId
            FieldOp::Int,    // numRanges
        ],
        verification: Verification::Prefix,
    },
    // The private ACOS wire shape for a `KeyCharacterMap` payload: the
    // keyboard type and the layout name. The Java reader is the
    // `KeyCharacterMap.nativeReadFromParcel` native, so the bytes only have to
    // agree with that native (and `nativeWriteToParcel`).
    ParcelableSpec {
        class: "android.view.KeyCharacterMap",
        fields: &[FieldOp::Int, FieldOp::Str8],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.hardware.input.HostUsiVersion",
        fields: &[FieldOp::Int, FieldOp::Int],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.hardware.OverlayProperties",
        fields: &[FieldOp::Bool, FieldOp::Bool],
        verification: Verification::Curated,
    },
    // Real API-34 `ParcelFileDescriptor.writeToParcel`:
    //   if (mCommFd != null) { writeInt(1); writeFileDescriptor(mFd); writeFileDescriptor(mCommFd); }
    //   else                { writeInt(0); writeFileDescriptor(mFd); }
    // The codec models the common no-`mCommFd` case (int marker + one fd); the
    // `$2.createFromParcel` reader reads exactly `readInt()` then
    // `readRawFileDescriptor()`. (A `mCommFd` descriptor pair is out of V1
    // scope; the scanner originally reported the prefix as a single `Fd`.)
    ParcelableSpec {
        class: "android.os.ParcelFileDescriptor",
        fields: &[FieldOp::Int, FieldOp::Fd],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.os.Bundle",
        fields: &[FieldOp::Bundle],
        verification: Verification::Curated,
    },
    // `MemoryIntArray.writeToParcel` writes a single `ParcelFileDescriptor`;
    // the Settings provider hands the framework a generation counter in this
    // shape (REPORT §3.7). The Java reader is `MemoryIntArray.CREATOR`.
    ParcelableSpec {
        class: "android.util.MemoryIntArray",
        fields: &[FieldOp::Named("android.os.ParcelFileDescriptor")],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.os.IBinder",
        fields: &[FieldOp::Int],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.view.textservice.SpellCheckerInfo",
        fields: &[FieldOp::Str16],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.view.textservice.SpellCheckerSubtype",
        fields: &[FieldOp::Int, FieldOp::Str16],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.app.ActivityManager$RunningAppProcessInfo",
        fields: &[FieldOp::Int, FieldOp::Str16],
        verification: Verification::Curated,
    },
    // `android.app.servertransaction.ClientTransaction.writeToParcel`:
    //   writeBoolean(mIsGlobal)
    //   writeStrongBinder(mActivityToken)
    //   writeParcelable(mLifecycleStateRequest)
    //   writeBoolean(mIsReady)
    //   writeParcelableList(mCallbacks)
    ParcelableSpec {
        class: "android.app.servertransaction.ClientTransaction",
        fields: &[
            FieldOp::Bool,
            FieldOp::Binder,
            FieldOp::Named("android.app.servertransaction.ActivityLifecycleItem"),
            FieldOp::Bool,
            FieldOp::ParcelableList("android.app.servertransaction.ClientTransactionItem"),
        ],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.content.AttributionSource",
        // `AttributionSource.writeToParcel` delegates to
        // `AttributionSourceState.writeToParcel`: a backpatched size int, then
        // pid, uid, packageName, attributionTag, a token binder,
        // renouncedPermissions and the `next` chain.
        fields: &[
            FieldOp::Int,   // size (backpatched)
            FieldOp::Int,   // pid
            FieldOp::Int,   // uid
            FieldOp::Str16, // packageName
            FieldOp::Str16, // attributionTag
            FieldOp::Binder,
            FieldOp::StringArray,
            FieldOp::TypedArray("android.content.AttributionSourceState"),
        ],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.content.AttributionSourceState",
        fields: &[
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Str16,
            FieldOp::Str16,
            FieldOp::Binder,
            FieldOp::StringArray,
            FieldOp::TypedArray("android.content.AttributionSourceState"),
        ],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.net.Uri",
        // `Uri.writeToParcel`: an int type tag (StringUri=1, Opaque=2,
        // Hierarchical=3) followed by `writeString8(toString())`. Verified in
        // `.cache/boot/android-34/dex/framework/classes.dex`
        // (`Landroid/net/Uri$HierarchicalUri;->writeToParcel`).
        fields: &[FieldOp::Int, FieldOp::Str8],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "com.android.internal.infra.AndroidFuture",
        fields: &[FieldOp::Int],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.view.accessibility.AccessibilityManager$ClientInfo",
        fields: &[FieldOp::Int],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.content.pm.ProviderInfoList",
        fields: &[FieldOp::StringArray],
        verification: Verification::Curated,
    },
    // `android.app.ProfilerInfo.writeToParcel`:
    //   writeString(profileFile)
    //   if (profileFd != null) { writeInt(1); profileFd.writeToParcel(...) }
    //   else writeInt(0)
    //   writeInt(samplingInterval); writeInt(autoStopProfiler ? 1 : 0);
    //   writeInt(streamingOutput ? 1 : 0); writeString(agent);
    //   writeBoolean(attachAgentDuringBind); writeInt(clockType);
    //   writeInt(profilerOutputVersion)
    ParcelableSpec {
        class: "android.app.ProfilerInfo",
        fields: &[
            FieldOp::Str16,
            FieldOp::Cond(&[FieldOp::Nested("android.os.ParcelFileDescriptor")]),
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Str16,
            FieldOp::Bool,
            FieldOp::Int, // clockType
        ],
        verification: Verification::Full,
    },
    // `android.content.res.CompatibilityInfo.writeToParcel` (API 34):
    //   writeInt(mCompatibilityFlags); writeInt(applicationDensity);
    //   writeFloat(applicationScale); writeFloat(applicationInvertedScale)
    ParcelableSpec {
        class: "android.content.res.CompatibilityInfo",
        fields: &[FieldOp::Int, FieldOp::Int, FieldOp::Float, FieldOp::Float],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.content.AutofillOptions",
        fields: &[FieldOp::Int],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.content.ContentCaptureOptions",
        fields: &[FieldOp::Int],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.os.SharedMemory",
        fields: &[FieldOp::Int],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.app.assist.AssistStructure",
        fields: &[FieldOp::Int],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.app.assist.AssistContent",
        fields: &[FieldOp::Int],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.content.ContentValues",
        fields: &[FieldOp::Bundle],
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.view.inputmethod.ImeTracker$Token",
        fields: &[FieldOp::Int, FieldOp::Str16],
        verification: Verification::Curated,
    },
    // `android.app.ResultInfo.writeToParcel` (API 34):
    //   writeString(mResultWho); writeInt(mRequestCode); writeInt(mResultCode);
    //   if (mData != null) { writeInt(1); mData.writeToParcel(dest, flags); }
    //   else writeInt(0);
    ParcelableSpec {
        class: "android.app.ResultInfo",
        fields: &[
            FieldOp::Str16,
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Cond(&[FieldOp::Nested("android.content.Intent")]),
        ],
        verification: Verification::Full,
    },
    // `android.os.PersistableBundle` is a `BaseBundle`: its wire payload is the
    // same `[length][magic][size][entries]` map as a `Bundle`, so the codec
    // reuses [`read_bundle`]/[`write_bundle`]. `Parcel.writePersistableBundle`
    // adds no marker (it writes the inner length directly, `-1` for null), so
    // the map layout is exactly the payload.
    ParcelableSpec {
        class: "android.os.PersistableBundle",
        fields: &[FieldOp::PersistableBundle],
        verification: Verification::Curated,
    },
    // `android.app.servertransaction.LaunchActivityItem.writeToParcel` (API 34).
    ParcelableSpec {
        class: "android.app.servertransaction.LaunchActivityItem",
        fields: &[
            FieldOp::Typed("android.content.Intent"),
            FieldOp::Int, // mIdent
            FieldOp::Typed("android.content.pm.ActivityInfo"),
            FieldOp::Typed("android.content.res.Configuration"),
            FieldOp::Typed("android.content.res.Configuration"),
            FieldOp::Int,  // mDeviceId
            FieldOp::Str16, // mReferrer
            FieldOp::Interface, // mVoiceInteractor
            FieldOp::Int,  // mProcState
            FieldOp::Bundle, // mState
            FieldOp::PersistableBundle, // mPersistentState
            FieldOp::TypedList("android.app.ResultInfo"), // mPendingResults
            FieldOp::TypedList("android.content.Intent"), // mPendingNewIntents
            FieldOp::Bundle, // mActivityOptions
            FieldOp::Bool, // mIsForward
            FieldOp::Typed("android.app.ProfilerInfo"),
            FieldOp::Binder, // mAssistToken
            FieldOp::Interface, // mActivityClientController
            FieldOp::Binder, // mShareableActivityToken
            FieldOp::Bool, // mLaunchedFromBubble
            FieldOp::Binder, // mTaskFragmentToken
        ],
        verification: Verification::Full,
    },
    // `ResumeActivityItem.writeToParcel`: mProcState, mUpdateProcState,
    // mIsForward, mShouldSendCompatFakeFocus.
    ParcelableSpec {
        class: "android.app.servertransaction.ResumeActivityItem",
        fields: &[FieldOp::Int, FieldOp::Bool, FieldOp::Bool, FieldOp::Bool],
        verification: Verification::Full,
    },
    // `ActivityRelaunchItem.writeToParcel`: pending results/new intents,
    // configChanges, `MergedConfiguration`, preserveWindow.
    ParcelableSpec {
        class: "android.app.servertransaction.ActivityRelaunchItem",
        fields: &[
            FieldOp::TypedList("android.app.ResultInfo"),
            FieldOp::TypedList("android.content.Intent"),
            FieldOp::Int, // mConfigChanges
            FieldOp::Typed("android.util.MergedConfiguration"),
            FieldOp::Bool, // mPreserveWindow
        ],
        verification: Verification::Full,
    },
    // `android.window.ClientWindowFrames.writeToParcel`.
    ParcelableSpec {
        class: "android.window.ClientWindowFrames",
        fields: &[
            FieldOp::Nested("android.graphics.Rect"), // frame
            FieldOp::Nested("android.graphics.Rect"), // displayFrame
            FieldOp::Nested("android.graphics.Rect"), // parentFrame
            FieldOp::Typed("android.graphics.Rect"),  // attachedFrame
            FieldOp::Bool, // isParentFrameClippedByDisplayCutout
            FieldOp::Float, // compatScale
        ],
        verification: Verification::Full,
    },
    // `android.util.MergedConfiguration.writeToParcel`: three bare
    // `Configuration.writeToParcel` payloads.
    ParcelableSpec {
        class: "android.util.MergedConfiguration",
        fields: &[
            FieldOp::Nested("android.content.res.Configuration"),
            FieldOp::Nested("android.content.res.Configuration"),
            FieldOp::Nested("android.content.res.Configuration"),
        ],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.graphics.Insets",
        fields: &[FieldOp::Int, FieldOp::Int, FieldOp::Int, FieldOp::Int],
        verification: Verification::Full,
    },
    // `android.view.InsetsSource.writeToParcel`.
    ParcelableSpec {
        class: "android.view.InsetsSource",
        fields: &[
            FieldOp::Int, // mId
            FieldOp::Int, // mType
            FieldOp::Nested("android.graphics.Rect"), // mFrame
            FieldOp::Cond(&[FieldOp::Nested("android.graphics.Rect")]), // mVisibleFrame
            FieldOp::Bool, // mVisible
            FieldOp::Int,  // mFlags
            FieldOp::Bool, // mInsetsRoundedCornerFrame
        ],
        verification: Verification::Full,
    },
    // `android.view.InsetsSourceControl.writeToParcel`.
    ParcelableSpec {
        class: "android.view.InsetsSourceControl",
        fields: &[
            FieldOp::Int,  // mId
            FieldOp::Int,  // mType
            FieldOp::Typed("android.view.SurfaceControl"), // mLeash
            FieldOp::Bool, // mInitiallyVisible
            FieldOp::Typed("android.graphics.Point"), // mSurfacePosition
            FieldOp::Typed("android.graphics.Insets"), // mInsetsHint
            FieldOp::Bool, // mSkipAnimationOnce
        ],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.view.InsetsSourceControl$Array",
        fields: &[FieldOp::TypedArray("android.view.InsetsSourceControl")],
        verification: Verification::Full,
    },
    // `android.view.InsetsState.writeToParcel`: display frame, cutout wrapper,
    // rounded corners, privacy indicator bounds, display shape, then the
    // `mSources` sparse array as a count + `writeTypedObject` loop.
    ParcelableSpec {
        class: "android.view.InsetsState",
        fields: &[
            FieldOp::Nested("android.graphics.Rect"),
            FieldOp::Nested("android.view.DisplayCutout$ParcelableWrapper"),
            FieldOp::Typed("android.view.RoundedCorners"),
            FieldOp::Nested("android.graphics.Rect"),
            FieldOp::Typed("android.view.PrivacyIndicatorBounds"),
            FieldOp::Typed("android.view.DisplayShape"),
            FieldOp::CountedTyped("android.view.InsetsSource"),
        ],
        verification: Verification::Full,
    },
    // `RoundedCorners.writeToParcel`: an int marker, then (only when not
    // `NO_ROUNDED_CORNERS`) a `writeTypedArray` of corners.
    ParcelableSpec {
        class: "android.view.RoundedCorners",
        fields: &[FieldOp::Cond(&[FieldOp::TypedArray("android.view.RoundedCorner")])],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.view.PrivacyIndicatorBounds",
        fields: &[FieldOp::TypedArray("android.graphics.Rect"), FieldOp::Int],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.view.DisplayShape",
        fields: &[
            FieldOp::Str8,
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Float,
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Float,
        ],
        verification: Verification::Full,
    },
    // `DisplayCutout$ParcelableWrapper.writeToParcel` delegates to the static
    // `writeCutoutToParcel`, which the scanner records as one nested write.
    ParcelableSpec {
        class: "android.view.DisplayCutout$ParcelableWrapper",
        fields: &[FieldOp::Nested("android.view.DisplayCutout")],
        verification: Verification::Full,
    },
    // `writeCutoutToParcel` (`DisplayCutout$ParcelableWrapper`) writes `-1` for
    // null, `0` for `NO_CUTOUT`, otherwise `1` followed by the safe insets, the
    // bounding rects, the waterfall insets and the path-parser info. The
    // private static helper is not a `writeToParcel` method, so the DEX
    // scanner cannot verify this table; the layout is transcribed from
    // `classes4.dex:Landroid/view/DisplayCutout$ParcelableWrapper;`.
    ParcelableSpec {
        class: "android.view.DisplayCutout",
        fields: &[FieldOp::Cond(&[
            FieldOp::Typed("android.graphics.Rect"),     // mSafeInsets
            FieldOp::TypedArray("android.graphics.Rect"), // mBounds rects
            FieldOp::Typed("android.graphics.Insets"),   // mWaterfallInsets
            FieldOp::Int,   // displayWidth
            FieldOp::Int,   // displayHeight
            FieldOp::Int,   // physicalDisplayWidth
            FieldOp::Int,   // physicalDisplayHeight
            FieldOp::Float, // density
            FieldOp::Str16, // cutoutSpec
            FieldOp::Int,   // rotation
            FieldOp::Float, // scale
            FieldOp::Float, // physicalPixelDisplaySizeRatio
        ])],
        verification: Verification::Curated,
    },
];

/// Look up a spec by dotted class name.
#[must_use]
pub fn spec_for(class: &str) -> Option<&'static ParcelableSpec> {
    SPECS.iter().find(|s| s.class == class)
}

/// True when a codec exists for `class`.
#[must_use]
pub fn has_codec(class: &str) -> bool {
    spec_for(class).is_some()
}

fn unsupported(class: &str) -> ServiceError {
    let _ = ErrorCode::UnsupportedAndroidApi;
    ServiceError::UnsupportedParcelable {
        class: class.to_string(),
    }
}

// ---------------------------------------------------------------------------
// generic spec engine
// ---------------------------------------------------------------------------

/// Read a parcelable payload (no `writeTypedObject` presence marker).
pub fn read_payload(class: &str, parcel: &mut Parcel) -> Result<Parcelable> {
    let spec = spec_for(class).ok_or_else(|| unsupported(class))?;
    let mut values = Vec::with_capacity(spec.fields.len());
    for field in spec.fields {
        values.push(read_field(*field, parcel)?);
    }
    Ok(Parcelable::Ordered {
        class: class.to_string(),
        values,
    })
}

/// Write a parcelable payload (no `writeTypedObject` presence marker).
pub fn write_payload(parcel: &mut Parcel, value: &Parcelable) -> Result<()> {
    let Parcelable::Ordered { class, values } = value else {
        return Err(ServiceError::BadReply {
            interface: "parcelable".into(),
            method: class_of(value).into(),
            detail: "null payload".into(),
        });
    };
    let spec = spec_for(class).ok_or_else(|| unsupported(class))?;
    if values.len() != spec.fields.len() {
        return Err(ServiceError::BadReply {
            interface: "parcelable".into(),
            method: class.clone(),
            detail: format!(
                "expected {} fields, got {}",
                spec.fields.len(),
                values.len()
            ),
        });
    }
    for (field, value) in spec.fields.iter().zip(values) {
        write_field(*field, parcel, value)?;
    }
    Ok(())
}

fn class_of(p: &Parcelable) -> &str {
    p.class().unwrap_or("null")
}

fn read_field(field: FieldOp, parcel: &mut Parcel) -> Result<Value> {
    Ok(match field {
        FieldOp::Int => Value::Int(parcel.read_int32()?),
        FieldOp::Long => Value::Long(parcel.read_int64()?),
        FieldOp::Float => Value::Float(parcel.read_float()?),
        FieldOp::Double => Value::Double(parcel.read_double()?),
        FieldOp::Bool => Value::Bool(parcel.read_bool()?),
        FieldOp::Byte => Value::Byte(parcel.read_byte()?),
        FieldOp::Char => Value::Char(parcel.read_char()?),
        FieldOp::Str16 => Value::Str(parcel.read_string16()?),
        FieldOp::CharSequence => {
            let kind = parcel.read_int32()?;
            if kind != 1 {
                return Err(unsupported("android.text.Spanned"));
            }
            Value::Str(parcel.read_string8()?)
        }
        FieldOp::Str8 => Value::Str8(parcel.read_string8()?),
        FieldOp::StringArray => Value::StringArray(parcel.read_string16_array()?),
        FieldOp::IntArray => Value::IntArray(parcel.read_int32_array()?),
        FieldOp::LongArray => Value::LongArray(parcel.read_int64_array()?),
        FieldOp::FloatArray => Value::FloatArray(parcel.read_float_array()?),
        FieldOp::ByteArray => Value::ByteArray(parcel.read_byte_array()?),
        FieldOp::SparseArray => {
            let n = parcel.read_int32()?;
            if n < 0 {
                Value::Skipped
            } else {
                return Err(unsupported("android.util.SparseArray"));
            }
        }
        FieldOp::Bundle => Value::Bundle(read_bundle(parcel)?),
        FieldOp::Fd => Value::RawFd(read_fd(parcel)?.map(RawFdValue)),
        FieldOp::Binder => Value::Binder(parcel.read_strong_binder()?),
        FieldOp::TypedArray(class) => {
            let n = parcel.read_int32()?;
            if n < 0 {
                Value::List(None)
            } else {
                let n = n as usize;
                // Each typed payload occupies at least one byte, so a count
                // larger than the remaining payload is malformed. Bound it
                // before `with_capacity` (host-safety, HONESTY_AUDIT #3).
                if n > parcel.remaining() {
                    return Err(ParcelError::BadLength {
                        len: n,
                        max: parcel.remaining(),
                    }
                    .into());
                }
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    out.push(Value::Parcelable(read_payload(class, parcel)?));
                }
                Value::List(Some(out))
            }
        }
        FieldOp::Nested(class) => Value::Parcelable(read_payload(class, parcel)?),
        FieldOp::ParcelableList(class) => {
            let n = parcel.read_int32()?;
            if n < 0 {
                Value::List(None)
            } else {
                let n = n as usize;
                if n > parcel.remaining() {
                    return Err(ParcelError::BadLength {
                        len: n,
                        max: parcel.remaining(),
                    }
                    .into());
                }
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    let name = parcel.read_string16()?;
                    match name {
                        None => out.push(Value::Parcelable(Parcelable::Null)),
                        Some(actual) => {
                            let _ = class;
                            out.push(Value::Parcelable(read_payload(&actual, parcel)?));
                        }
                    }
                }
                Value::List(Some(out))
            }
        }
        FieldOp::Dynamic => Value::Dynamic(read_dynamic(parcel)?),
        FieldOp::Cond(fields) => {
            let present = parcel.read_int32()? != 0;
            let mut values = Vec::new();
            if present {
                for field in fields {
                    values.push(read_field(*field, parcel)?);
                }
            }
            Value::Cond { present, values }
        }
        FieldOp::PersistableBundle => Value::Bundle(read_bundle(parcel)?),
        FieldOp::TypedList(class) => {
            let n = parcel.read_int32()?;
            if n < 0 {
                Value::List(None)
            } else {
                let n = n as usize;
                if n > parcel.remaining() {
                    return Err(ParcelError::BadLength {
                        len: n,
                        max: parcel.remaining(),
                    }
                    .into());
                }
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    if parcel.read_int32()? == 0 {
                        out.push(Value::Parcelable(Parcelable::Null));
                    } else {
                        out.push(Value::Parcelable(read_payload(class, parcel)?));
                    }
                }
                Value::List(Some(out))
            }
        }
        FieldOp::Interface => Value::Binder(parcel.read_strong_binder()?),
        FieldOp::CountedTyped(class) => {
            let n = parcel.read_int32()?;
            if n < 0 {
                Value::List(None)
            } else {
                let n = n as usize;
                if n > parcel.remaining() {
                    return Err(ParcelError::BadLength {
                        len: n,
                        max: parcel.remaining(),
                    }
                    .into());
                }
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    if parcel.read_int32()? == 0 {
                        out.push(Value::Parcelable(Parcelable::Null));
                    } else {
                        out.push(Value::Parcelable(read_payload(class, parcel)?));
                    }
                }
                Value::List(Some(out))
            }
        }
        FieldOp::Unsupported(class) => return Err(unsupported(class)),
        FieldOp::Named(class) | FieldOp::IgnoredNamed(class) => {
            let name = parcel.read_string16()?;
            match name {
                None => Value::NamedParcelable(Parcelable::Null),
                Some(actual) => {
                    let payload =
                        read_payload(&actual, parcel).map_err(|_| unsupported(&actual))?;
                    let _ = class;
                    Value::NamedParcelable(payload)
                }
            }
        }
        FieldOp::Typed(class) | FieldOp::IgnoredTyped(class) => {
            let present = parcel.read_int32()?;
            if present == 0 {
                Value::TypedObject(Parcelable::Null)
            } else if class == "android.os.IBinder" {
                let binder = parcel.read_strong_binder()?;
                Value::TypedObject(Parcelable::Ordered {
                    class: class.to_string(),
                    values: vec![Value::Int(binder.map(|b| b.0 as i32).unwrap_or(-1))],
                })
            } else {
                Value::TypedObject(read_payload(class, parcel)?)
            }
        }
    })
}

fn write_field(field: FieldOp, parcel: &mut Parcel, value: &Value) -> Result<()> {
    match field {
        FieldOp::Int => parcel.write_int32(expect_int(value)?),
        FieldOp::Long => parcel.write_int64(expect_long(value)?),
        FieldOp::Float => parcel.write_float(expect_float(value)?),
        FieldOp::Double => parcel.write_double(expect_double(value)?),
        FieldOp::Bool => parcel.write_bool(expect_bool(value)?),
        FieldOp::Byte => parcel.write_byte(expect_byte(value)?),
        FieldOp::Char => parcel.write_char(expect_char(value)?),
        FieldOp::Str16 => parcel.write_string16_opt(expect_str(value)?)?,
        FieldOp::CharSequence => {
            parcel.write_int32(1);
            parcel.write_string8_opt(expect_str(value)?)?;
        }
        FieldOp::Str8 => parcel.write_string8_opt(expect_str(value)?)?,
        FieldOp::StringArray => {
            let arr = expect_string_array(value)?;
            let refs: Option<Vec<&str>> = arr.map(|v| v.iter().map(|s| s.as_deref().unwrap_or("")).collect());
            parcel.write_string16_array(refs.as_deref())?;
        }
        FieldOp::IntArray => parcel.write_int32_array(expect_int_array(value)?)?,
        FieldOp::LongArray => parcel.write_int64_array(expect_long_array(value)?)?,
        FieldOp::FloatArray => parcel.write_float_array(expect_float_array(value)?)?,
        FieldOp::ByteArray => parcel.write_byte_array(expect_byte_array(value)?)?,
        FieldOp::SparseArray => match value {
            Value::Skipped | Value::List(None) => parcel.write_int32(-1),
            _ => return Err(unsupported("android.util.SparseArray")),
        },
        FieldOp::Bundle => write_bundle(parcel, expect_bundle(value)?)?,
        FieldOp::Fd => write_fd(parcel, value)?,
        FieldOp::Binder => parcel.write_strong_binder_opt(value.as_binder()),
        FieldOp::TypedArray(_class) => match value {
            Value::List(None) | Value::Skipped => parcel.write_int32(-1),
            Value::List(Some(items)) => {
                parcel.write_int32(
                    i32::try_from(items.len()).map_err(|_| bad("typed array too large"))?,
                );
                for item in items {
                    write_payload(parcel, expect_parcelable(item)?)?;
                }
            }
            _ => return Err(bad(format!("expected typed array, got {value:?}"))),
        },
        FieldOp::Nested(class) => {
            let _ = class;
            write_payload(parcel, expect_parcelable(value)?)?;
        }
        FieldOp::ParcelableList(_class) => match value {
            Value::List(None) | Value::Skipped => parcel.write_int32(-1),
            Value::List(Some(items)) => {
                parcel.write_int32(
                    i32::try_from(items.len()).map_err(|_| bad("parcelable list too large"))?,
                );
                for item in items {
                    let p = expect_parcelable(item)?;
                    match p {
                        Parcelable::Null => parcel.write_string16_opt(None)?,
                        Parcelable::Ordered { class, .. } => {
                            let _ = class;
                            parcel.write_string16(class)?;
                            write_payload(parcel, p)?;
                        }
                        Parcelable::Fd(_) => {
                            return Err(bad("parcelable list element is an fd"))
                        }
                    }
                }
            }
            _ => return Err(bad(format!("expected parcelable list, got {value:?}"))),
        },
        FieldOp::Dynamic => write_dynamic(parcel, expect_dynamic(value)?)?,
        FieldOp::Cond(fields) => {
            let Value::Cond { present, values } = value else {
                return Err(bad(format!("expected conditional, got {value:?}")));
            };
            parcel.write_int32(i32::from(*present));
            if *present {
                if values.len() != fields.len() {
                    return Err(bad(format!(
                        "conditional expects {} fields, got {}",
                        fields.len(),
                        values.len()
                    )));
                }
                for (field, value) in fields.iter().zip(values) {
                    write_field(*field, parcel, value)?;
                }
            }
        }
        FieldOp::PersistableBundle => write_bundle(parcel, expect_bundle(value)?)?,
        FieldOp::TypedList(class) => match value {
            Value::List(None) | Value::Skipped => parcel.write_int32(-1),
            Value::List(Some(items)) => {
                parcel.write_int32(
                    i32::try_from(items.len()).map_err(|_| bad("typed list too large"))?,
                );
                for item in items {
                    let p = expect_parcelable(item)?;
                    match p {
                        Parcelable::Null => parcel.write_int32(0),
                        Parcelable::Ordered { .. } => {
                            parcel.write_int32(1);
                            let _ = class;
                            write_payload(parcel, p)?;
                        }
                        Parcelable::Fd(_) => return Err(bad("typed list element is an fd")),
                    }
                }
            }
            _ => return Err(bad(format!("expected typed list, got {value:?}"))),
        },
        FieldOp::Interface => parcel.write_strong_binder_opt(value.as_binder()),
        FieldOp::CountedTyped(_class) => match value {
            Value::List(None) | Value::Skipped => parcel.write_int32(-1),
            Value::List(Some(items)) => {
                parcel.write_int32(
                    i32::try_from(items.len()).map_err(|_| bad("counted typed list too large"))?,
                );
                for item in items {
                    let p = expect_parcelable(item)?;
                    match p {
                        Parcelable::Null => parcel.write_int32(0),
                        Parcelable::Ordered { .. } => {
                            parcel.write_int32(1);
                            write_payload(parcel, p)?;
                        }
                        Parcelable::Fd(_) => return Err(bad("counted typed element is an fd")),
                    }
                }
            }
            _ => return Err(bad(format!("expected counted typed list, got {value:?}"))),
        },
        FieldOp::Unsupported(class) => return Err(unsupported(class)),
        FieldOp::Named(_) | FieldOp::IgnoredNamed(_) => {
            let p = expect_parcelable(value)?;
            match p {
                Parcelable::Null => parcel.write_string16_opt(None)?,
                Parcelable::Ordered { class, .. } => {
                    parcel.write_string16(class)?;
                    write_payload(parcel, p)?;
                }
                Parcelable::Fd(_) => return Err(bad("named parcelable is an fd")),
            }
        }
        FieldOp::Typed(class) | FieldOp::IgnoredTyped(class) => {
            let p = expect_parcelable(value)?;
            match p {
                Parcelable::Null => parcel.write_int32(0),
                Parcelable::Ordered { .. } => {
                    parcel.write_int32(1);
                    if class == "android.os.IBinder" {
                        let id = p
                            .field(0)
                            .and_then(Value::as_int)
                            .and_then(|v| u64::try_from(v).ok())
                            .map(acos_parcel::BinderId);
                        parcel.write_strong_binder_opt(id);
                    } else {
                        write_payload(parcel, p)?;
                    }
                }
                Parcelable::Fd(_) => return Err(bad("typed parcelable is an fd")),
            }
        }
    }
    Ok(())
}

fn bad(detail: impl Into<String>) -> ServiceError {
    ServiceError::BadReply {
        interface: "parcelable".into(),
        method: "field".into(),
        detail: detail.into(),
    }
}

fn expect_int(v: &Value) -> Result<i32> {
    v.as_int().ok_or_else(|| bad(format!("expected int, got {v:?}")))
}
fn expect_long(v: &Value) -> Result<i64> {
    v.as_long().ok_or_else(|| bad(format!("expected long, got {v:?}")))
}
fn expect_float(v: &Value) -> Result<f32> {
    v.as_float().ok_or_else(|| bad(format!("expected float, got {v:?}")))
}
fn expect_double(v: &Value) -> Result<f64> {
    match v {
        Value::Double(d) => Ok(*d),
        _ => Err(bad(format!("expected double, got {v:?}"))),
    }
}
fn expect_bool(v: &Value) -> Result<bool> {
    v.as_bool().ok_or_else(|| bad(format!("expected bool, got {v:?}")))
}
fn expect_byte(v: &Value) -> Result<i8> {
    match v {
        Value::Byte(b) => Ok(*b),
        _ => Err(bad(format!("expected byte, got {v:?}"))),
    }
}
fn expect_char(v: &Value) -> Result<u16> {
    match v {
        Value::Char(c) => Ok(*c),
        _ => Err(bad(format!("expected char, got {v:?}"))),
    }
}
fn expect_str(v: &Value) -> Result<Option<&str>> {
    match v {
        Value::Str(s) | Value::Str8(s) => Ok(s.as_deref()),
        _ => Err(bad(format!("expected string, got {v:?}"))),
    }
}
fn expect_string_array(v: &Value) -> Result<Option<&Vec<Option<String>>>> {
    match v {
        Value::StringArray(a) => Ok(a.as_ref()),
        _ => Err(bad(format!("expected string array, got {v:?}"))),
    }
}
fn expect_int_array(v: &Value) -> Result<Option<&[i32]>> {
    match v {
        Value::IntArray(a) => Ok(a.as_deref()),
        _ => Err(bad(format!("expected int array, got {v:?}"))),
    }
}
fn expect_long_array(v: &Value) -> Result<Option<&[i64]>> {
    match v {
        Value::LongArray(a) => Ok(a.as_deref()),
        _ => Err(bad(format!("expected long array, got {v:?}"))),
    }
}
fn expect_float_array(v: &Value) -> Result<Option<&[f32]>> {
    match v {
        Value::FloatArray(a) => Ok(a.as_deref()),
        _ => Err(bad(format!("expected float array, got {v:?}"))),
    }
}
fn expect_byte_array(v: &Value) -> Result<Option<&[u8]>> {
    match v {
        Value::ByteArray(a) => Ok(a.as_deref()),
        _ => Err(bad(format!("expected byte array, got {v:?}"))),
    }
}
fn expect_bundle(v: &Value) -> Result<Option<&Bundle>> {
    match v {
        Value::Bundle(b) => Ok(b.as_ref()),
        _ => Err(bad(format!("expected bundle, got {v:?}"))),
    }
}
fn expect_dynamic(v: &Value) -> Result<&crate::value::DynamicValue> {
    match v {
        Value::Dynamic(d) => Ok(d),
        _ => Err(bad(format!("expected dynamic value, got {v:?}"))),
    }
}
fn expect_parcelable(v: &Value) -> Result<&Parcelable> {
    v.as_parcelable()
        .ok_or_else(|| bad(format!("expected parcelable, got {v:?}")))
}

// ---------------------------------------------------------------------------
// Bundle (scalar/string subset)
// ---------------------------------------------------------------------------

/// `Parcel.VAL_*` tags used by the subset ACOS models.
pub mod val {
    /// `VAL_NULL`.
    pub const NULL: i32 = -1;
    /// `VAL_STRING`.
    pub const STRING: i32 = 0;
    /// `VAL_INTEGER`.
    pub const INTEGER: i32 = 1;
    /// `VAL_LONG`.
    pub const LONG: i32 = 6;
    /// `VAL_FLOAT`.
    pub const FLOAT: i32 = 7;
    /// `VAL_DOUBLE`.
    pub const DOUBLE: i32 = 8;
    /// `VAL_BOOLEAN`.
    pub const BOOLEAN: i32 = 9;
    /// `VAL_BYTE`.
    pub const BYTE: i32 = 20;
    /// `VAL_BYTEARRAY`.
    pub const BYTEARRAY: i32 = 13;
    /// `VAL_INTARRAY`.
    pub const INTARRAY: i32 = 18;
    /// `VAL_STRINGARRAY`.
    pub const STRINGARRAY: i32 = 14;
    /// `VAL_MAP`.
    pub const MAP: i32 = 2;
    /// `VAL_BUNDLE`.
    pub const BUNDLE: i32 = 3;
    /// `VAL_IBINDER`.
    pub const IBINDER: i32 = 15;
    /// `VAL_PARCELABLE`.
    pub const PARCELABLE: i32 = 4;
}

/// Read a `writeBundle` payload.
///
/// Android's `BaseBundle.writeToParcelInner` writes a 4-byte-aligned byte
/// length, then the `ArrayMap` size and entries (`Bundle.readBundle` checks the
/// alignment). ACOS models exactly that layout so the real framework reads it.
pub fn read_bundle(parcel: &mut Parcel) -> Result<Option<Bundle>> {
    let length = parcel.read_int32()?;
    if length < 0 {
        return Ok(None);
    }
    // Android 14 `BaseBundle` writes a `BUNDLE_MAGIC` header after the length.
    let magic = parcel.read_int32()?;
    if magic as u32 != BUNDLE_MAGIC {
        return Err(ServiceError::BadRequest {
            interface: "bundle".into(),
            method: "readBundle".into(),
            detail: format!("bad Bundle magic {magic:#x}"),
        });
    }
    let size = parcel.read_int32()?;
    if size < 0 {
        return Ok(None);
    }
    let mut bundle = Bundle::new();
    for _ in 0..size {
        let key = match parcel.read_string16()? {
            Some(k) => k,
            None => continue,
        };
        let value = read_dynamic(parcel)?;
        bundle.put(key, dynamic_to_value(value));
    }
    Ok(Some(bundle))
}

/// The Android 14 `BaseBundle` magic header (`"BNDL"`).
pub const BUNDLE_MAGIC: u32 = 0x4c44_4e42;

/// Write a `writeBundle` payload in the real `BaseBundle` layout.
/// Android's `String.hashCode` (`s[0]*31^(n-1) + ...`). `ArrayMap` keeps its
/// `mHashes` sorted by this value, and `Parcel.readArrayMap` appends entries in
/// wire order, so the writer must emit them in ascending-hash order.
fn java_string_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32))
}

pub fn write_bundle(parcel: &mut Parcel, bundle: Option<&Bundle>) -> Result<()> {
    match bundle {
        None => {
            parcel.write_int32(-1);
            Ok(())
        }
        Some(b) => {
            // Write `[length][magic][size][entries]` directly so file
            // descriptors in the map stay in this parcel's object table (an
            // `appendFrom` of a scratch parcel would lose them). The length is
            // backpatched, exactly like `BaseBundle.writeToParcelInner`.
            //
            // Entries are emitted in `ArrayMap` order: ascending by the key's
            // Java `hashCode`. `Parcel.readArrayMap` calls `ArrayMap.append` in
            // wire order and warns ("New hash ... is before end of array hash")
            // when that order is violated, so insertion order is not enough.
            let start = parcel.data_size();
            parcel.write_int32(0);
            parcel.write_int32(BUNDLE_MAGIC as i32);
            let payload_start = parcel.data_size();
            parcel.write_int32(i32::try_from(b.len()).map_err(|_| bad("bundle too large"))?);
            let mut entries: Vec<&(String, Value)> = b.entries.iter().collect();
            entries.sort_by_key(|(key, _)| java_string_hash(key));
            for (key, value) in entries {
                parcel.write_string16(key)?;
                write_dynamic(parcel, &value_to_dynamic(value)?)?;
            }
            let payload_end = parcel.data_size();
            let len = payload_end - payload_start;
            parcel.set_data_position(start);
            parcel.write_int32(i32::try_from(len).map_err(|_| bad("bundle too large"))?);
            parcel.set_data_position(payload_end);
            Ok(())
        }
    }
}

fn dynamic_to_value(v: crate::value::DynamicValue) -> Value {
    use crate::value::DynamicValue as D;
    match v {
        D::Null => Value::Skipped,
        D::String(s) => Value::Str(s),
        D::Int(i) => Value::Int(i),
        D::Long(l) => Value::Long(l),
        D::Float(f) => Value::Float(f),
        D::Double(d) => Value::Double(d),
        D::Bool(b) => Value::Bool(b),
        D::Byte(b) => Value::Byte(b),
        D::ByteArray(a) => Value::ByteArray(a),
        D::IntArray(a) => Value::IntArray(a),
        D::LongArray(a) => Value::LongArray(a),
        D::StringArray(a) => Value::StringArray(a),
        D::Binder(b) => Value::Binder(b),
        D::Map(m) => Value::Map(Some(m)),
        D::Parcelable(p) => Value::Parcelable(p),
        D::Skipped { .. } => Value::Skipped,
    }
}

fn value_to_dynamic(v: &Value) -> Result<crate::value::DynamicValue> {
    use crate::value::DynamicValue as D;
    Ok(match v {
        Value::Str(s) => D::String(s.clone()),
        Value::Str8(s) => D::String(s.clone()),
        Value::Int(i) => D::Int(*i),
        Value::Long(l) => D::Long(*l),
        Value::Float(f) => D::Float(*f),
        Value::Double(d) => D::Double(*d),
        Value::Bool(b) => D::Bool(*b),
        Value::Byte(b) => D::Byte(*b),
        Value::ByteArray(a) => D::ByteArray(a.clone()),
        Value::IntArray(a) => D::IntArray(a.clone()),
        Value::LongArray(a) => D::LongArray(a.clone()),
        Value::StringArray(a) => D::StringArray(a.clone()),
        Value::Binder(b) => D::Binder(*b),
        Value::Parcelable(p) | Value::TypedObject(p) | Value::NamedParcelable(p) => {
            D::Parcelable(p.clone())
        }
        Value::RawFd(Some(fd)) => D::Parcelable(Parcelable::Fd(Some(
            fd.0.try_clone().map_err(ServiceError::Parcel)?,
        ))),
        Value::RawFd(None) => D::Null,
        Value::Bundle(Some(b)) => D::Parcelable(b.parcelable()),
        Value::Bundle(None) => D::Null,
        Value::Map(Some(entries)) => D::Map(entries.clone()),
        Value::Map(None) => D::Null,
        Value::Skipped | Value::Unit => D::Null,
        other => return Err(bad(format!("bundle value not dynamic: {other:?}"))),
    })
}

/// Read a `writeValue` payload.
pub fn read_dynamic(parcel: &mut Parcel) -> Result<crate::value::DynamicValue> {
    use crate::value::DynamicValue as D;
    let tag = parcel.read_int32()?;
    Ok(match tag {
        val::NULL => D::Null,
        val::STRING => D::String(parcel.read_string16()?),
        val::INTEGER => D::Int(parcel.read_int32()?),
        val::LONG => D::Long(parcel.read_int64()?),
        val::FLOAT => D::Float(parcel.read_float()?),
        val::DOUBLE => D::Double(parcel.read_double()?),
        val::BOOLEAN => D::Bool(parcel.read_bool()?),
        val::BYTE => D::Byte(parcel.read_byte()?),
        val::BYTEARRAY => D::ByteArray(parcel.read_byte_array()?),
        val::INTARRAY => D::IntArray(parcel.read_int32_array()?),
        val::STRINGARRAY => {
            D::StringArray(parcel.read_string16_array()?)
        }
        val::IBINDER => D::Binder(parcel.read_strong_binder()?),
        val::PARCELABLE => {
            // `Parcel.writeValue` length-prefixes `VAL_PARCELABLE`.
            let _len = parcel.read_int32()?;
            let class = parcel.read_string16()?.ok_or_else(|| ServiceError::BadRequest {
                interface: "bundle".into(),
                method: "readValue".into(),
                detail: "VAL_PARCELABLE with a null class name".into(),
            })?;
            D::Parcelable(read_payload(&class, parcel)?)
        }
        val::MAP => {
            // `Parcel.readHashMap`: a size (-1 for null) then alternating
            // key/value `writeValue` payloads. `Settings$NameValueCache`
            // reads the `LIST_config` reply's `value` key this way.
            let size = parcel.read_int32()?;
            if size < 0 {
                D::Null
            } else {
                let mut entries = Vec::with_capacity(size as usize);
                for _ in 0..size {
                    let k = read_dynamic(parcel)?;
                    let v = read_dynamic(parcel)?;
                    entries.push((k, v));
                }
                D::Map(entries)
            }
        }
        val::BUNDLE => {
            let b = read_bundle(parcel)?;
            D::Map(
                b.map(|b| {
                    b.entries
                        .into_iter()
                        .map(|(k, v)| {
                            (
                                D::String(Some(k)),
                                value_to_dynamic(&v).unwrap_or(D::Null),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            )
        }
        other => {
            // We cannot know the payload width of an unmodelled tag. Fail loudly
            // rather than desynchronising the rest of the parcel.
            return Err(ServiceError::BadRequest {
                interface: "bundle".into(),
                method: "readValue".into(),
                detail: format!("unmodelled Parcel.VAL_* tag {other}"),
            });
        }
    })
}

/// Write a `writeValue` payload.
pub fn write_dynamic(parcel: &mut Parcel, value: &crate::value::DynamicValue) -> Result<()> {
    use crate::value::DynamicValue as D;
    match value {
        D::Null => parcel.write_int32(val::NULL),
        D::String(s) => {
            parcel.write_int32(val::STRING);
            parcel.write_string16_opt(s.as_deref())?;
        }
        D::Int(i) => {
            parcel.write_int32(val::INTEGER);
            parcel.write_int32(*i);
        }
        D::Long(l) => {
            parcel.write_int32(val::LONG);
            parcel.write_int64(*l);
        }
        D::Float(f) => {
            parcel.write_int32(val::FLOAT);
            parcel.write_float(*f);
        }
        D::Double(d) => {
            parcel.write_int32(val::DOUBLE);
            parcel.write_double(*d);
        }
        D::Bool(b) => {
            parcel.write_int32(val::BOOLEAN);
            parcel.write_bool(*b);
        }
        D::Byte(b) => {
            parcel.write_int32(val::BYTE);
            parcel.write_byte(*b);
        }
        D::ByteArray(a) => {
            parcel.write_int32(val::BYTEARRAY);
            parcel.write_byte_array(a.as_deref())?;
        }
        D::IntArray(a) => {
            parcel.write_int32(val::INTARRAY);
            parcel.write_int32_array(a.as_deref())?;
        }
        D::StringArray(a) => {
            parcel.write_int32(val::STRINGARRAY);
            let refs: Option<Vec<&str>> =
                a.as_ref().map(|v| v.iter().map(|s| s.as_deref().unwrap_or("")).collect());
            parcel.write_string16_array(refs.as_deref())?;
        }
        D::Binder(b) => {
            parcel.write_int32(val::IBINDER);
            parcel.write_strong_binder_opt(*b);
        }
        D::LongArray(a) => {
            parcel.write_int32(val::NULL);
            let _ = a;
            return Err(bad("VAL_LONGARRAY is not modelled"));
        }
        D::Parcelable(p) => {
            parcel.write_int32(val::PARCELABLE);
            // `Parcel.writeValue` length-prefixes `VAL_PARCELABLE`; backpatch it.
            let start = parcel.data_size();
            parcel.write_int32(0);
            let payload_start = parcel.data_size();
            match p {
                Parcelable::Null => parcel.write_string16_opt(None)?,
                Parcelable::Ordered { class, .. } => {
                    parcel.write_string16(class)?;
                    write_payload(parcel, p)?;
                }
                Parcelable::Fd(fd) => {
                    // Match `ParcelFileDescriptor.writeToParcel`: the `mCommFd`
                    // presence marker followed by the fd (see the spec above).
                    parcel.write_string16("android.os.ParcelFileDescriptor")?;
                    parcel.write_int32(0);
                    write_fd(
                        parcel,
                        &Value::RawFd(
                            fd.as_ref()
                                .and_then(|f| f.try_clone().ok())
                                .map(RawFdValue),
                        ),
                    )?;
                }
            }
            let end = parcel.data_size();
            let len = end - payload_start;
            parcel.set_data_position(start);
            parcel.write_int32(i32::try_from(len).map_err(|_| bad("parcelable too large"))?);
            parcel.set_data_position(end);
        }
        D::Map(entries) => {
            // `Parcel.writeMap`: size then alternating `writeValue` pairs. A
            // `HashMap` is written as `VAL_MAP` (it is a `Map`, so
            // `writeValue` never reaches the `Serializable` branch).
            parcel.write_int32(val::MAP);
            parcel.write_int32(i32::try_from(entries.len()).map_err(|_| bad("map too large"))?);
            for (k, v) in entries {
                write_dynamic(parcel, k)?;
                write_dynamic(parcel, v)?;
            }
        }
        D::Skipped { tag } => {
            return Err(bad(format!("cannot re-write skipped VAL_* tag {tag}")));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// file descriptors
// ---------------------------------------------------------------------------

fn read_fd(parcel: &mut Parcel) -> Result<Option<acos_parcel::ParcelFd>> {
    match parcel.read_file_descriptor()? {
        None => Ok(None),
        Some(raw) => {
            // The parcel owns the descriptor; wrap a duplicate for the caller.
            let borrowed = parcel
                .borrowed_file_descriptor(raw)
                .ok_or_else(|| bad("parcel does not own the fd it returned"))?;
            acos_parcel::ParcelFd::dup(borrowed)
                .map(Some)
                .map_err(ServiceError::Parcel)
        }
    }
}

fn write_fd(parcel: &mut Parcel, value: &Value) -> Result<()> {
    match value {
        Value::RawFd(Some(fd)) => {
            parcel
                .write_dup_file_descriptor(fd.as_fd())
                .map_err(ServiceError::Parcel)
        }
        Value::RawFd(None) | Value::Skipped => {
            parcel.write_int32(0);
            Ok(())
        }
        _ => Err(bad(format!("expected fd, got {value:?}"))),
    }
}

// ---------------------------------------------------------------------------
// DEX verification oracle
// ---------------------------------------------------------------------------

/// The `Parcel.write*` names a `writeToParcel` method calls, in order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScannedWrite {
    /// `Parcel.write*` method names (`writeInt`, `writeString8`, ...).
    pub calls: Vec<String>,
    /// Set when a non-`Parcel` delegate was seen (`super.writeToParcel`, ...).
    pub delegates: Vec<String>,
}

/// A tiny pull helper so callers do not have to depend on `acos-dex` directly.
pub struct Scanner;

impl Scanner {
    /// Scan one class's `writeToParcel` from the Android 14 boot DEX directory.
    pub fn write_to_parcel(
        dex_dir: &std::path::Path,
        class: &str,
    ) -> std::result::Result<ScannedWrite, String> {
        scan::write_to_parcel(dex_dir, class)
    }
}

/// Lazily cached boot DEX, used by [`verify_prefix`] tests.
static BOOT_DEX: OnceLock<Option<Vec<acos_dex::DexFile>>> = OnceLock::new();

/// Load `.cache/boot/android-34/dex/framework` if present.
#[must_use]
pub fn boot_dex() -> Option<&'static [acos_dex::DexFile]> {
    BOOT_DEX
        .get_or_init(|| {
            let dir = crate::workspace_path(".cache/boot/android-34/dex/framework")?;
            let dir = dir.as_path();
            let mut paths: Vec<_> = std::fs::read_dir(dir)
                .ok()?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "dex").unwrap_or(false))
                .collect();
            paths.sort();
            let mut out = Vec::new();
            for p in paths {
                let bytes = std::fs::read(p).ok()?;
                if let Ok(dex) = acos_dex::DexFile::parse(bytes) {
                    out.push(dex);
                }
            }
            Some(out)
        })
        .as_deref()
}

/// Verify that a spec's field kinds are a prefix of the real method.
///
/// Returns `(matched, real_len)`.
#[must_use]
pub fn verify_prefix(spec: &ParcelableSpec, scanned: &ScannedWrite) -> (usize, usize) {
    let v = verify(spec, scanned);
    (v.matched_calls, v.real_calls)
}

fn field_matches(field: FieldOp, call: &str) -> bool {
    match field {
        FieldOp::Int => call == "writeInt",
        FieldOp::Long => call == "writeLong",
        FieldOp::Float => call == "writeFloat",
        FieldOp::Double => call == "writeDouble",
        FieldOp::Bool => call == "writeBoolean",
        FieldOp::Byte => call == "writeByte",
        FieldOp::Char => call == "writeChar",
        FieldOp::Str16 => call == "writeString" || call == "writeString16",
        // The scanner records `TextUtils.writeToParcel` as a single
        // `writeString` op, so a `CharSequence` field matches that one call
        // even though the wire form is `int + writeString8`.
        FieldOp::CharSequence => call == "writeString",
        FieldOp::Str8 => call == "writeString8",
        FieldOp::StringArray => call == "writeString8Array" || call == "writeStringArray",
        FieldOp::IntArray => call == "writeIntArray",
        FieldOp::LongArray => call == "writeLongArray",
        FieldOp::FloatArray => call == "writeFloatArray",
        FieldOp::ByteArray => call == "writeByteArray",
        FieldOp::SparseArray => call == "writeSparseArray",
        FieldOp::Named(_) | FieldOp::IgnoredNamed(_) => call == "writeParcelable",
        FieldOp::Typed(_) | FieldOp::IgnoredTyped(_) => call == "writeTypedObject",
        FieldOp::Bundle => call == "writeBundle",
        FieldOp::Fd => call == "writeFileDescriptor",
        FieldOp::Binder => call == "writeStrongBinder",
        FieldOp::TypedArray(_) => call == "writeTypedArray",
        FieldOp::Nested(_) => call == "writeParcelable",
        FieldOp::ParcelableList(_) => call == "writeParcelableList",
        FieldOp::Dynamic => call == "writeValue",
        FieldOp::PersistableBundle => call == "writePersistableBundle",
        FieldOp::TypedList(_) => call == "writeTypedList",
        FieldOp::Interface => call == "writeStrongInterface",
        // The presence marker is a `writeInt`; the branch body is invisible to a
        // linear scanner, so only the marker can be verified.
        FieldOp::Cond(_) => call == "writeInt",
        FieldOp::CountedTyped(_) => call == "writeInt",
        FieldOp::Unsupported(_) => false,
    }
}

/// Outcome of verifying a spec against a scanned `writeToParcel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verify {
    /// Number of real `Parcel.write*` calls the spec consumed.
    pub matched_calls: usize,
    /// Number of real calls in the scanned method (after branch collapse).
    pub real_calls: usize,
    /// True when every spec field was consumed (no unconsumed field).
    pub all_fields: bool,
}

impl Verify {
    /// True when the spec accounts for every real call and has no spare field.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.all_fields && self.matched_calls == self.real_calls
    }
}

/// Number of calls `field` consumes from `calls` (0 when it does not match).
fn field_consumes(field: FieldOp, calls: &[String]) -> usize {
    let Some(first) = calls.first() else {
        return 0;
    };
    match field {
        FieldOp::Cond(inner) => {
            if first != "writeInt" {
                return 0;
            }
            let mut n = 1;
            for f in inner {
                let c = field_consumes(*f, &calls[n..]);
                if c == 0 {
                    return n;
                }
                n += c;
            }
            n
        }
        FieldOp::CountedTyped(_) => {
            if first != "writeInt" {
                return 0;
            }
            match calls.get(1).map(String::as_str) {
                Some("writeTypedObject") => 2,
                _ => 1,
            }
        }
        _ => usize::from(field_matches(field, first)),
    }
}

/// Verify a spec against a scanned `writeToParcel`.
#[must_use]
pub fn verify(spec: &ParcelableSpec, scanned: &ScannedWrite) -> Verify {
    let mut matched_calls = 0;
    let mut all_fields = true;
    for field in spec.fields {
        let consumed = field_consumes(*field, &scanned.calls[matched_calls..]);
        if consumed == 0 {
            all_fields = false;
            break;
        }
        matched_calls += consumed;
    }
    Verify {
        matched_calls,
        real_calls: scanned.calls.len(),
        all_fields,
    }
}

/// Compact class-name-only description used by `results/services`.
#[must_use]
pub fn codec_table() -> Vec<(&'static str, usize, &'static str)> {
    SPECS
        .iter()
        .map(|s| {
            let v = match s.verification {
                Verification::Full => "full",
                Verification::Prefix => "prefix",
                Verification::Curated => "curated",
            };
            (s.class, s.fields.len(), v)
        })
        .collect()
}

/// The scanner implementation (kept here so the crate has no `acos-dex` leak).
pub mod scan {
    use acos_dex::decode::{decode, payload_kind, payload_len};
    use acos_dex::{ClassDef, DexFile};

    use super::ScannedWrite;

    /// Scan one class's `writeToParcel` from a boot DEX directory.
    pub fn write_to_parcel(
        dex_dir: &std::path::Path,
        class: &str,
    ) -> Result<ScannedWrite, String> {
        let desc = format!("L{};", class.replace('.', "/"));
        let mut paths: Vec<_> = std::fs::read_dir(dex_dir)
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "dex").unwrap_or(false))
            .collect();
        paths.sort();
        for path in paths {
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let Ok(dex) = DexFile::parse(bytes) else {
                continue;
            };
            if let Some(def) = dex.find_class(&desc) {
                return scan_class(&dex, def);
            }
        }
        Err(format!("class not found: {class}"))
    }

    /// Control-flow terminator of a scanned region.
    #[derive(Clone, Copy)]
    enum Term {
        /// The region fell through to its `stop` (or the end of the method).
        Fall,
        /// The region ended in an unconditional `goto` to this target.
        Goto(usize),
    }

    fn scan_class(dex: &DexFile, def: &ClassDef) -> Result<ScannedWrite, String> {
        let mut out = ScannedWrite::default();
        let desc = dex
            .class_descriptor(def)
            .map_err(|e| e.to_string())?
            .to_string();
        let mut visited = std::collections::HashSet::new();
        scan_method(dex, &desc, "writeToParcel", &mut visited, &mut out)?;
        if out.calls.is_empty() {
            return Err("no writeToParcel".into());
        }
        Ok(out)
    }

    /// Scan `class_desc`'s `method`, appending the `Parcel.write*` calls in
    /// execution order. `invoke-super` and direct nested `writeToParcel` calls
    /// are spliced in so the op list is the full inherited field order.
    fn scan_method(
        dex: &DexFile,
        class_desc: &str,
        method: &str,
        visited: &mut std::collections::HashSet<String>,
        out: &mut ScannedWrite,
    ) -> Result<(), String> {
        let key = format!("{class_desc}->{method}");
        if !visited.insert(key) {
            return Ok(());
        }
        let Some(def) = dex.find_class(class_desc) else {
            return Ok(());
        };
        let Some(cd) = dex.class_data(def).map_err(|e| e.to_string())? else {
            return Ok(());
        };
        for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
            let Ok(r) = dex.method_ref(m.method_idx) else {
                continue;
            };
            if r.name != method || m.code_off == 0 {
                continue;
            }
            let code = dex.code_item(m.code_off).map_err(|e| e.to_string())?;
            scan_region(dex, &code.insns, 0, None, visited, out)?;
            return Ok(());
        }
        Ok(())
    }

    /// Walk the instructions of one region in execution order. Conditional
    /// branches (if/else and if-then) are collapsed to a single canonical arm:
    /// the longer `if`/`else` arm is preferred, and loops contribute their body
    /// once before the exit edge. This mirrors what the runtime actually writes
    /// and is what makes `Configuration.userSetLocale` (a `writeInt` in both
    /// arms) and `ProfilerInfo.profileFd` (a presence marker + nested write)
    /// verifiable.
    fn scan_region(
        dex: &DexFile,
        insns: &[u16],
        mut pc: usize,
        stop: Option<usize>,
        visited: &mut std::collections::HashSet<String>,
        out: &mut ScannedWrite,
    ) -> Result<Term, String> {
        let mut guard = 0usize;
        while pc < insns.len() {
            guard += 1;
            if guard > 20_000 {
                break;
            }
            if let Some(s) = stop {
                if pc == s {
                    return Ok(Term::Fall);
                }
            }
            if payload_kind(insns, pc).is_some() {
                pc += payload_len(insns, pc).unwrap_or(1);
                continue;
            }
            let Ok(insn) = decode(insns, pc) else {
                pc += 1;
                continue;
            };
            if insn.opcode.is_goto() {
                let target = (pc as i64 + i64::from(insn.target)) as usize;
                if stop.is_some() {
                    // An unconditional arm end; its target is the join point.
                    return Ok(Term::Goto(target));
                }
                if target <= pc {
                    // A backward `goto` at the top level is a loop tail.
                    return Ok(Term::Fall);
                }
                pc = target;
                continue;
            }
            if insn.opcode.is_branch() {
                let fall = pc + insn.size as usize;
                let take = (pc as i64 + i64::from(insn.target)) as usize;
                let mut fall_out = ScannedWrite::default();
                let fall_term = scan_region(dex, insns, fall, Some(take), visited, &mut fall_out)?;
                match fall_term {
                    Term::Fall => {
                        // if-then: the fall arm is the conditional body.
                        out.calls.extend(fall_out.calls);
                        out.delegates.extend(fall_out.delegates);
                        pc = take;
                    }
                    Term::Goto(join) => {
                        if join <= pc {
                            // Loop: one body pass, then the loop exit edge.
                            out.calls.extend(fall_out.calls);
                            out.delegates.extend(fall_out.delegates);
                            pc = take;
                            continue;
                        }
                        let mut take_out = ScannedWrite::default();
                        let _ = scan_region(dex, insns, take, Some(join), visited, &mut take_out)?;
                        if take_out.calls.len() > fall_out.calls.len() {
                            out.calls.extend(take_out.calls);
                            out.delegates.extend(take_out.delegates);
                        } else {
                            out.calls.extend(fall_out.calls);
                            out.delegates.extend(fall_out.delegates);
                        }
                        pc = join;
                    }
                }
                continue;
            }
            if insn.opcode.is_terminator() {
                return Ok(Term::Fall);
            }
            if insn.opcode.is_invoke() {
                record_invoke(dex, insn.index, insn.opcode.byte(), visited, out)?;
            }
            pc += insn.size as usize;
        }
        Ok(Term::Fall)
    }

    fn record_invoke(
        dex: &DexFile,
        index: u32,
        opcode: u8,
        visited: &mut std::collections::HashSet<String>,
        out: &mut ScannedWrite,
    ) -> Result<(), String> {
        let Ok(m) = dex.method_ref(index) else {
            return Ok(());
        };
        if m.class == "Landroid/os/Parcel;" && m.name.starts_with("write") {
            if m.name != "writeInterfaceToken"
                && m.name != "writeNoException"
                && m.name != "writeStackTrace"
                && m.name != "writeException"
            {
                out.calls.push(m.name.to_string());
            }
        } else if m.class == "Landroid/os/Parcel;" && m.name == "maybeWriteSquashed" {
            out.calls.push("writeInt".to_string());
        } else if m.class == "Landroid/text/TextUtils;" && m.name == "writeToParcel" {
            out.calls.push("writeString".to_string());
        } else if m.class.starts_with("Lcom/android/internal/util/Parcelling") && m.name == "parcel" {
            // `Parcelling.BuiltIn.ForBoolean.parcel` writes one int (null→1,
            // false→0, true→-1); `ForStringSet.parcel` writes an int count then
            // the strings (null→-1). Only the leading int is visible to a
            // linear scan.
            out.calls.push("writeInt".to_string());
        } else if m.name == "writeToParcel" && opcode == 0x6f {
            // `invoke-super`: inline the inherited field order.
            scan_method(dex, m.class, "writeToParcel", visited, out)?;
        } else if m.name == "writeToParcel" || m.name == "writeCutoutToParcel" {
            // A nested Parcelable written through its own `writeToParcel`
            // (not `Parcel.writeParcelable`). Record it as a `writeParcelable`
            // op so a spec's `FieldOp::Nested` can verify the position.
            out.calls.push("writeParcelable".to_string());
            out.delegates.push(m.class.to_string());
        }
        Ok(())
    }
}
