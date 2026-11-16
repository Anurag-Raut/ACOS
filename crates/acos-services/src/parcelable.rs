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
    /// `writeStringArray` (`Parcel.writeString16Array`): an `int` count (`-1`
    /// for null) followed by each string as a **String16**.
    StringArray,
    /// `writeString8Array`: an `int` count (`-1` for null) followed by each
    /// string as a **String8**. Every `android.content.pm` parcelable uses this
    /// form for its `String[]` fields (`PackageInfo.splitNames`,
    /// `requestedPermissions`; `ApplicationInfo.split*`; `ComponentInfo.
    /// attributionTags`); the bytes differ from [`FieldOp::StringArray`] for
    /// every non-empty array, so the two are distinct ops, not aliases.
    String8Array,
    /// `writeStringList`: an `int` count (`-1` for null) followed by each
    /// string. Wire-identical to [`FieldOp::StringArray`]; separate so the
    /// decoded value is a [`Value::StringList`], which is what the Java reader
    /// builds (`Parcel.readStringList(ArrayList)`).
    StringList,
    /// `writeIntArray`.
    IntArray,
    /// `writeLongArray`.
    LongArray,
    /// `writeFloatArray`.
    FloatArray,
    /// `writeByteArray`.
    ByteArray,
    /// Bytes written verbatim (no length prefix, no padding added). This is for
    /// payloads the peer's own native code reads directly from the parcel, not
    /// through a Java `writeToParcel`: the image's
    /// `KeyCharacterMap.nativeReadFromParcel` / `readFromParcel` is the one
    /// user (W16). Reading such a payload back is unsupported, because its
    /// length is not on the wire.
    Raw,
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
    /// A conditional group with a one-byte presence marker: the API-34
    /// `NotificationChannel`/`NotificationChannelGroup` write every nullable
    /// field as `writeByte(0|1)` and only then the payload. Same body rule as
    /// [`FieldOp::Cond`].
    ByteCond(&'static [FieldOp]),
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
    /// A `writeInt` count followed by that many **bare** `writeToParcel`
    /// payloads (no presence marker, no class name). API-34
    /// `DisplayInfo.writeToParcel` writes `supportedModes` this way:
    /// `dest.writeInt(supportedModes.length)` then
    /// `supportedModes[i].writeToParcel(dest, flags)`, and `readFromParcel`
    /// mirrors it with `Display.Mode.CREATOR.createFromParcel(source)`.
    CountedNested(&'static str),
    /// A `writeInt` count followed by that many repetitions of a field
    /// sequence. This is the general form of the `writeInt(size); for (…)`
    /// loop AOSP writes inline or through a private helper:
    /// `NetworkCapabilities.writeParcelableArraySet` (`writeInt`, then a
    /// `writeParcelable` per element) and `LinkProperties.writeAddresses`
    /// (`writeInt`, then `writeByteArray` + `writeBoolean` + `writeInt` per
    /// `InetAddress`). The DEX oracle collapses a loop to one body pass, so a
    /// value consumes the body when the scanned calls continue with it and
    /// only the count when they do not (an empty or `null` list). Each
    /// element's values are carried as
    /// [`Value::Cond`]` { present: true, values }` in a [`Value::List`].
    Counted(&'static [FieldOp]),
    /// `writeList`: an `int` count (`-1` for `null`) followed by each element as
    /// a `writeValue` (API-34 `LinkProperties.mStackedLinks`). ACOS only writes
    /// the empty list, but a non-empty one round-trips through
    /// [`Value::Dynamic`].
    ValueList,
    /// One `LinkProperties.writeAddress` payload that has no list count
    /// (`mDhcpServerAddress`): a `writeByteArray`, then — for an
    /// `Inet6Address` — `writeBoolean(hasScopeId)` + `writeInt(scopeId)`.
    /// The IPv6 arm is dead for the dhcp field (the reader casts the address to
    /// `Inet4Address`), but the DEX oracle collapses the branch and sees the two
    /// calls, so they are matched while a `null`/IPv4 value writes only the byte
    /// array (which is exactly the framework's bytes for that value).
    Address,
    /// `DisplayCutout$ParcelableWrapper.writeCutoutToParcel`: an `int` variant
    /// marker (`-1` null, `0` `NO_CUTOUT`, `1` full cutout) and, for variant
    /// `1`, the full geometry. See [`read_cutout`]/[`write_cutout`].
    Cutout,
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

/// The dotted class name of `android.view.DisplayInfo` (the codec key and the
/// name [`crate::display::DisplayInfo::to_parcelable`] writes).
pub const DISPLAY_INFO_CLASS: &str = "android.view.DisplayInfo";

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
            FieldOp::String8Array, // splitNames
            FieldOp::String8Array, // splitSourceDirs
            FieldOp::String8Array, // splitPublicSourceDirs
            FieldOp::SparseArray, // splitDependencies
            FieldOp::Str8, // nativeLibraryDir
            FieldOp::Str8, // secondaryNativeLibraryDir
            FieldOp::Str8, // nativeLibraryRootDir
            FieldOp::Int,  // nativeLibraryRootRequiresIsa
            FieldOp::Str8, // primaryCpuAbi
            FieldOp::Str8, // secondaryCpuAbi
            FieldOp::String8Array, // resourceDirs
            FieldOp::String8Array, // overlayPaths
            FieldOp::Str8, // seInfo
            FieldOp::Str8, // seInfoUser
            FieldOp::String8Array, // sharedLibraryFiles
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
            FieldOp::String8Array, // splitClassLoaderNames
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
            FieldOp::String8Array, // attributionTags
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
            FieldOp::String8Array,
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
    //
    // `splitNames` and `requestedPermissions` are **`writeString8Array`**:
    // `PackageInfo.java:585,608` in the android-14 tag and
    // `Landroid/content/pm/PackageInfo;->writeToParcel` at `0x000b`/`0x0072` in
    // the image's `classes.dex`. Encoding them as String16 arrays (the old
    // `FieldOp::StringArray`) desynchronised every field after
    // `requestedPermissions`; the framework then read a `Signature[]` length
    // out of string payload bytes and threw
    // `OutOfMemoryError: Failed to allocate a 7676737936 byte allocation at
    // Parcel.createString8Array` for `getPackageInfo(pkg, 0x100024c0)`.
    ParcelableSpec {
        class: "android.content.pm.PackageInfo",
        fields: &[
            FieldOp::Str8,        // packageName
            FieldOp::String8Array, // splitNames
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
            FieldOp::String8Array, // requestedPermissions
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
    // `android.content.pm.Signature.writeToParcel`: one `writeByteArray`
    // (the DER X.509 certificate). Read back by `PackageInfo.signatures`'s
    // `TypedArray` and by `Signature.CREATOR`.
    ParcelableSpec {
        class: "android.content.pm.Signature",
        fields: &[FieldOp::ByteArray],
        verification: Verification::Full,
    },
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
            FieldOp::String8Array, // attributionTags
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
            FieldOp::String8Array, // attributionTags
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
            FieldOp::String8Array, // splitNames
            FieldOp::String8Array, // splitSourceDirs
            FieldOp::String8Array, // splitPublicSourceDirs
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
    // `FeatureInfo.writeToParcel` (API 34, `FeatureInfo.java`):
    // `writeString8(name); writeInt(version); writeInt(reqGlEsVersion);
    // writeInt(flags)`.
    ParcelableSpec {
        class: "android.content.pm.FeatureInfo",
        fields: &[
            FieldOp::Str8, // name
            FieldOp::Int,  // version
            FieldOp::Int,  // reqGlEsVersion
            FieldOp::Int,  // flags
        ],
        verification: Verification::Full,
    },
    // `android.app.ContentProviderHolder.writeToParcel` (API 34): the
    // `ProviderInfo` payload with **no** presence marker (the framework calls
    // `info.writeToParcel(dest, 0)` directly), the provider as a strong binder,
    // the `ContentProviderConnection` token as a strong binder, then
    // `noReleaseNeeded` and `mLocal`. The earlier spec wrote a typed
    // `ProviderInfo` and a typed `IBinder`, which shifted every following field
    // by one int and hid the connection token.
    ParcelableSpec {
        class: "android.app.ContentProviderHolder",
        fields: &[
            FieldOp::Nested("android.content.pm.ProviderInfo"),
            FieldOp::Binder, // provider (IContentProvider)
            FieldOp::Binder, // connection (ContentProviderConnection)
            // `writeInt(noReleaseNeeded ? 1 : 0)` and `writeInt(mLocal ? 1 : 0)`
            // — `writeInt`, not `writeBoolean` (the DEX scan is authoritative).
            FieldOp::Int, // noReleaseNeeded
            FieldOp::Int, // mLocal
        ],
        verification: Verification::Full,
    },
    // DisplayInfo: the scalar leading order is transcribed from
    // `classes4.dex:Landroid/view/DisplayInfo;->writeToParcel`. The nested
    // `DisplayCutout`, `DisplayShape`, supported `Mode[]` and the trailing
    // `SparseArray` are modelled with ACOS-owned placeholders (gap list).
    // DisplayInfo: the **full** API-34 `writeToParcel`, transcribed line by line
    // from `android-14.0.0_r75`
    // `frameworks/base/core/java/android/view/DisplayInfo.java:587-647` and read
    // back in the mirror order of `readFromParcel`
    // (`DisplayInfo.java:520-584`). The two 2024-era `IgnoredNamed`
    // placeholders that desynchronised the framework's own reader are gone: the
    // cutout is written by [`FieldOp::Cutout`] and `supportedModes` by
    // [`FieldOp::CountedNested`].
    //
    // `Verification::Prefix` because the DEX scanner collapses the two `for`
    // loops (`supportedModes`, `supportedColorModes`) and the explicit
    // `writeInt(length)` of the first one; the field order is instead pinned by
    // `tests/displayinfo_api34.rs::displayinfo_spec_is_the_api34_write_order`
    // and proven byte-for-byte against a hand-built parcel.
    ParcelableSpec {
        class: DISPLAY_INFO_CLASS,
        fields: &[
            FieldOp::Int,  // layerStack
            FieldOp::Int,  // flags
            FieldOp::Int,  // type
            FieldOp::Int,  // displayId
            FieldOp::Int,  // displayGroupId
            FieldOp::Named("android.view.DisplayAddress"), // address
            FieldOp::Named("android.hardware.display.DeviceProductInfo"), // deviceProductInfo
            FieldOp::Str8, // name
            FieldOp::Int,  // appWidth
            FieldOp::Int,  // appHeight
            FieldOp::Int,  // smallestNominalAppWidth
            FieldOp::Int,  // smallestNominalAppHeight
            FieldOp::Int,  // largestNominalAppWidth
            FieldOp::Int,  // largestNominalAppHeight
            FieldOp::Int,  // logicalWidth
            FieldOp::Int,  // logicalHeight
            FieldOp::Cutout, // DisplayCutout.ParcelableWrapper.writeCutoutToParcel
            FieldOp::Int,  // rotation
            FieldOp::Int,  // modeId
            FieldOp::Float, // renderFrameRate
            FieldOp::Int,  // defaultModeId
            FieldOp::Int,  // userPreferredModeId
            FieldOp::CountedNested("android.view.Display$Mode"), // supportedModes
            FieldOp::Int,  // colorMode
            FieldOp::IntArray, // supportedColorModes
            FieldOp::Named("android.view.Display$HdrCapabilities"), // hdrCapabilities
            FieldOp::Bool, // minimalPostProcessingSupported
            FieldOp::Int,  // logicalDensityDpi
            FieldOp::Float, // physicalXDpi
            FieldOp::Float, // physicalYDpi
            FieldOp::Long, // appVsyncOffsetNanos
            FieldOp::Long, // presentationDeadlineNanos
            FieldOp::Int,  // state
            FieldOp::Int,  // committedState
            FieldOp::Int,  // ownerUid
            FieldOp::Str8, // ownerPackageName
            FieldOp::Str8, // uniqueId
            FieldOp::Int,  // removeMode
            FieldOp::Float, // refreshRateOverride
            FieldOp::Float, // brightnessMinimum
            FieldOp::Float, // brightnessMaximum
            FieldOp::Float, // brightnessDefault
            FieldOp::Typed("android.view.RoundedCorners"), // roundedCorners
            FieldOp::IntArray, // userDisabledHdrTypes
            FieldOp::Int,  // installOrientation
            FieldOp::Typed("android.view.DisplayShape"), // displayShape
            FieldOp::Typed("android.view.SurfaceControl$RefreshRateRange"), // layoutLimitedRefreshRate
            FieldOp::Float, // hdrSdrRatio
            FieldOp::SparseArray, // thermalRefreshRateThrottling
            FieldOp::Str8, // thermalBrightnessThrottlingDataId
        ],
        verification: Verification::Prefix,
    },
    // `android.view.Display$Mode.writeToParcel`.
    //
    // The **image** build wins over the tag here: `android-14.0.0_r75`
    // `Display.java:2375-2382` writes `mVsyncRate` between `mPeakRefreshRate`
    // and `mAlternativeRefreshRates`, but the API-34 image's
    // `classes4.dex:Landroid/view/Display$Mode;->writeToParcel` has exactly six
    // `Parcel.write*` calls and no `writeFloat(mVsyncRate)` (verified with
    // `examples/scan-parcelables`). ACOS follows the image, because the image's
    // reader is the one that has to stay in sync.
    ParcelableSpec {
        class: "android.view.Display$Mode",
        fields: &[
            FieldOp::Int,        // mModeId
            FieldOp::Int,        // mWidth
            FieldOp::Int,        // mHeight
            FieldOp::Float,      // mPeakRefreshRate
            FieldOp::FloatArray, // mAlternativeRefreshRates
            FieldOp::IntArray,   // mSupportedHdrTypes
        ],
        verification: Verification::Full,
    },
    // `android.view.DisplayShape.writeToParcel` (DisplayShape.java:247-256).
    ParcelableSpec {
        class: "android.view.DisplayShape",
        fields: &[
            FieldOp::Str8,  // mDisplayShapeSpec
            FieldOp::Int,   // mDisplayWidth
            FieldOp::Int,   // mDisplayHeight
            FieldOp::Float, // mPhysicalPixelDisplaySizeRatio
            FieldOp::Int,   // mRotation
            FieldOp::Int,   // mOffsetX
            FieldOp::Int,   // mOffsetY
            FieldOp::Float, // mScale
        ],
        verification: Verification::Full,
    },
    // `android.view.SurfaceControl$RefreshRateRange.writeToParcel`
    // (SurfaceControl.java:2013-2016).
    ParcelableSpec {
        class: "android.view.SurfaceControl$RefreshRateRange",
        fields: &[FieldOp::Float, FieldOp::Float],
        verification: Verification::Full,
    },
    // `android.graphics.Insets.writeToParcel` (Insets.java:181-186); the cutout's
    // `waterfallInsets`.
    ParcelableSpec {
        class: "android.graphics.Insets",
        fields: &[FieldOp::Int, FieldOp::Int, FieldOp::Int, FieldOp::Int],
        verification: Verification::Full,
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
            // NOTE: the image's `InputDevice(Parcel)` has **no mDeviceBus** here
            // (read from the image's classes4.dex; the r75 tag does), so a field
            // for it desynchronises every later field and stops the first
            // traversal. Do not add one.
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
            // The image's reader stops after the motion ranges: no view
            // behaviour boolean on the wire.
        ],
        verification: Verification::Prefix,
    },
    // The image's native `KeyCharacterMap` payload (W16). The Java reader is
    // `KeyCharacterMap.nativeReadFromParcel` -> `KeyCharacterMap::readFromParcel`
    // in the image's libinput: `int32 deviceId`, `bool hasMap`, a raw
    // NUL-terminated+4-byte-padded `loadFileName` (`Parcel::writeCString`, no
    // length prefix), then the native map (type, overlay flag, keys with
    // behaviour lists, remap tables). It is not a Java `writeToParcel`, so the
    // bytes come from `keychars::KeyMap::parcel_bytes` (parsed from the stock
    // `/system/usr/keychars/*.kcm` the sysroot ships) and are written verbatim.
    ParcelableSpec {
        class: "android.view.KeyCharacterMap",
        fields: &[FieldOp::Raw],
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
    // `android.content.pm.ProviderInfoList.writeToParcel` (image
    // `classes.dex`): `allowSquashing()` (writes nothing), then
    // `writeTypedList(providers, flags)`, then `restoreAllowSquashing`. The old
    // `[StringArray]` spec described a class that has no string array at all;
    // no ACOS reply uses this codec yet (`atms` sends a null typed object), so
    // the wrong table was inert but wrong.
    ParcelableSpec {
        class: "android.content.pm.ProviderInfoList",
        fields: &[FieldOp::TypedList("android.content.pm.ProviderInfo")],
        verification: Verification::Full,
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
    // `classes4.dex:Landroid/view/DisplayCutout$ParcelableWrapper;`:
    // `readCutoutFromParcel` (`0x192ac4`) reads
    //
    //   int variant  (-1 null / 0 NO_CUTOUT / else present)
    //   readTypedObject(Rect)          safe insets
    //   `new Rect[4]` + readTypedArray(mBounds rects)
    //   readTypedObject(Insets)        waterfall insets
    //   int,int,int,int, float, String16, int, float, float
    //
    // **The image writes no `mSideOverrides` int array** (`android-14.0.0_r75`
    // does), and the bounds array is not `createTypedArray`: the reader
    // preallocates `Rect[4]`, so `Parcel.readTypedArray` throws
    // `RuntimeException("bad array lengths")` unless the wire count is exactly
    // 4. Both facts are image-only; the tag transcription is wrong for this
    // build.
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
    // W26: `IActivityManager.handleApplicationWtf` carries an
    // `ApplicationErrorReport$ParcelableCrashInfo`, which has no
    // `writeToParcel` of its own — it inherits `CrashInfo`'s
    // (`ParcelableCrashInfo` is a `Parcelable` wrapper). Field order read from
    // the image's `classes.dex`,
    // `Landroid/app/ApplicationErrorReport$CrashInfo;->writeToParcel`:
    //
    //   writeString(exceptionHandlerClassName)
    //   writeString(exceptionClassName)
    //   writeString(exceptionMessage)
    //   writeString(throwFileName)
    //   writeString(throwClassName)
    //   writeString(throwMethodName)
    //   writeInt(throwLineNumber)
    //   writeString(stackTrace)
    //   writeString(crashTag)          <- API 34 only (not in older tags)
    //
    // `readFromParcel` (`CrashInfo(Parcel)`, same dex) mirrors it field for
    // field, so the wire is order-exact and has no presence markers.
    ParcelableSpec {
        class: "android.app.ApplicationErrorReport$ParcelableCrashInfo",
        fields: &[
            FieldOp::Str16, // exceptionHandlerClassName
            FieldOp::Str16, // exceptionClassName
            FieldOp::Str16, // exceptionMessage
            FieldOp::Str16, // throwFileName
            FieldOp::Str16, // throwClassName
            FieldOp::Str16, // throwMethodName
            FieldOp::Int,   // throwLineNumber
            FieldOp::Str16, // stackTrace
            FieldOp::Str16, // crashTag
        ],
        verification: Verification::Curated,
    },
    // W28: `android.content.IntentFilter` — the argument of
    // `IActivityManager.registerReceiver[WithFeature]` (codes 18/19). Read from
    // the **image's** `classes.dex`,
    // `Landroid/content/IntentFilter;->writeToParcel(Landroid/os/Parcel;I)V`
    // (and mirrored by `-><init>(Landroid/os/Parcel;)V`, which reads it back
    // with `readStringList`/`readInt`/`PatternMatcher(Parcel)`):
    //
    //   writeStringArray(mActions.toArray())                // count + String16s
    //   if (mCategories != null) { writeInt(1); writeStringList(mCategories); }
    //   else                                writeInt(0);
    //   ... the same presence+list pair for mDataSchemes, mStaticDataTypes,
    //       mDataTypes and mMimeGroups (this build has both type lists and
    //       mime groups; the r75 tag does not)
    //   if (mDataSchemeSpecificParts != null) writeInt(1), count, each
    //       PatternMatcher.writeToParcel(dest, flags); else writeInt(0)
    //   if (mDataAuthorities != null)         writeInt(1), count, each
    //       IntentFilter$AuthorityEntry.writeToParcel(dest); else writeInt(0)
    //   if (mDataPaths != null)               writeInt(1), count, each
    //       PatternMatcher.writeToParcel(dest, flags); else writeInt(0)
    //   writeInt(mPriority)
    //   writeInt(mHasStaticPartialTypes)        // boolean written as int
    //   writeInt(mHasDynamicPartialTypes)
    //   writeInt(getAutoVerify())
    //   writeInt(mInstantAppVisibility)
    //   writeInt(mOrder)
    //   if (mExtras != null) { writeInt(1); mExtras.writeToParcel(dest, flags); }
    //   else                 writeInt(0);
    //
    // There is no required permission, title or group id on this wire (the tag
    // has them; the image does not). `writeStringArray` and `writeStringList`
    // share one wire shape, so the action array and the optional lists are the
    // same `int count` + `String16` sequence.
    ParcelableSpec {
        class: "android.os.PatternMatcher",
        fields: &[
            FieldOp::Str16,    // mPattern (writeString)
            FieldOp::Int,      // mType
            FieldOp::IntArray, // mParsedPattern
        ],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.content.IntentFilter$AuthorityEntry",
        fields: &[
            FieldOp::Str16, // mOrigHost (writeString)
            FieldOp::Str16, // mHost (writeString)
            FieldOp::Int,   // mWild (boolean as int)
            FieldOp::Int,   // mPort
        ],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.content.IntentFilter",
        fields: &[
            FieldOp::StringArray,                              // mActions
            FieldOp::Cond(&[FieldOp::StringList]),             // mCategories
            FieldOp::Cond(&[FieldOp::StringList]),             // mDataSchemes
            FieldOp::Cond(&[FieldOp::StringList]),             // mStaticDataTypes
            FieldOp::Cond(&[FieldOp::StringList]),             // mDataTypes
            FieldOp::Cond(&[FieldOp::StringList]),             // mMimeGroups
            FieldOp::Cond(&[FieldOp::CountedNested("android.os.PatternMatcher")]),
            FieldOp::Cond(&[FieldOp::CountedNested(
                "android.content.IntentFilter$AuthorityEntry",
            )]),
            FieldOp::Cond(&[FieldOp::CountedNested("android.os.PatternMatcher")]),
            FieldOp::Int,                            // mPriority
            FieldOp::Int,                            // mHasStaticPartialTypes
            FieldOp::Int,                            // mHasDynamicPartialTypes
            FieldOp::Int,                            // getAutoVerify()
            FieldOp::Int,                            // mInstantAppVisibility
            FieldOp::Int,                            // mOrder
            FieldOp::Cond(&[FieldOp::PersistableBundle]), // mExtras
        ],
        verification: Verification::Full,
    },
    // ---- K2/R3: read-only query replies -------------------------------------
    ParcelableSpec {
        class: "android.app.ActivityManager$MemoryInfo",
        fields: &[
            FieldOp::Long, // availMem
            FieldOp::Long, // totalMem
            FieldOp::Long, // threshold
            FieldOp::Long, // advertisedMem
            FieldOp::Int,  // lowMemory
            FieldOp::Long, // hiddenAppThreshold
            FieldOp::Long, // secondaryServerThreshold
            FieldOp::Long, // visibleAppThreshold
            FieldOp::Long, // foregroundAppThreshold
        ],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.content.pm.InstallSourceInfo",
        fields: &[
            FieldOp::Str16, // initiatingPackageName
            FieldOp::Named("android.content.pm.SigningInfo"),
            FieldOp::Str16, // originatingPackageName
            FieldOp::Str16, // localPackageName
            FieldOp::Str8,  // packageSource
            FieldOp::Int,   // initiatingPackageSigningInfo
        ],
        verification: Verification::Prefix,
    },
    ParcelableSpec {
        class: "android.net.Network",
        fields: &[FieldOp::Int],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.os.Messenger",
        fields: &[FieldOp::Binder],
        verification: Verification::Full,
    },
    // ---- NET1: connectivity (API-34 `ConnectivityManager` wire) ------------
    //
    // `NetworkCapabilities.writeToParcel` (packages/modules/Connectivity,
    // android-14.0.0_r75, bytecode in
    // `dex/framework-connectivity/classes.dex`) writes:
    //
    // ```text
    // writeLong ×3, writeInt ×2, writeParcelable ×2, writeInt,
    // writeParcelableArraySet(mUids), writeIntArray, writeString, writeBoolean,
    // writeIntArray, writeInt ×2, writeString, writeIntArray, writeTypedList,
    // writeInt
    // ```
    //
    // `writeParcelableArraySet` is a private helper (`writeInt(size)` then a
    // `writeParcelable` per element) and the oracle now inlines it, which is
    // [`FieldOp::Counted`]. The two nullable `Parcelable` fields are
    // `writeParcelable` — a `null` one writes only the `null` class-name
    // string, but the field must not be modelled as a bare `writeString` or the
    // next real writer change (a non-null specifier) desynchronises silently.
    // `mUnderlyingNetworks` is `writeTypedList` (surfaces in the scan) and its
    // `null` encoding is `writeInt(-1)`, which [`FieldOp::TypedList`] writes.
    ParcelableSpec {
        class: "android.net.NetworkCapabilities",
        fields: &[
            FieldOp::Long,     // mNetworkCapabilities & ALL_VALID_CAPABILITIES
            FieldOp::Long,     // mForbiddenNetworkCapabilities
            FieldOp::Long,     // mTransportTypes & ALL_VALID_TRANSPORTS
            FieldOp::Int,      // mLinkUpBandwidthKbps
            FieldOp::Int,      // mLinkDownBandwidthKbps
            FieldOp::Named("android.net.NetworkSpecifier"), // mNetworkSpecifier
            FieldOp::Named("android.net.TransportInfo"),    // mTransportInfo
            FieldOp::Int,      // mSignalStrength
            FieldOp::Counted(&[FieldOp::Named("android.net.UidRange")]), // mUids ArraySet
            FieldOp::IntArray, // mAllowedUids
            FieldOp::Str16,    // mSSID
            FieldOp::Bool,     // mPrivateDnsBroken
            FieldOp::IntArray, // administrator uids
            FieldOp::Int,      // mOwnerUid
            FieldOp::Int,      // mRequestorUid
            FieldOp::Str16,    // mRequestorPackageName
            FieldOp::IntArray, // mSubIds
            FieldOp::TypedList("android.net.Network"), // mUnderlyingNetworks
            FieldOp::Int,      // mEnterpriseId & ALL_VALID_ENTERPRISE_IDS
        ],
        verification: Verification::Full,
    },
    // `NetworkRequest.writeToParcel`: the capabilities (a bare payload, no
    // class-name prefix), the legacy type, the request id and the `Type` name
    // (`Type.valueOf(readString())` in the creator).
    ParcelableSpec {
        class: "android.net.NetworkRequest",
        fields: &[
            FieldOp::Nested("android.net.NetworkCapabilities"),
            FieldOp::Int,   // legacyType
            FieldOp::Int,   // requestId
            FieldOp::Str16, // type.name()
        ],
        verification: Verification::Full,
    },
    // `LinkProperties.writeToParcel` (android-14.0.0_r75, bytecode in
    // `dex/framework-connectivity/classes.dex`): interface name, three address
    // lists (each `writeInt(count)` then a `writeParcelable`/`writeAddress` per
    // element; the helpers are inlined by the oracle now), the private-DNS
    // facts, domains, the dhcp server address, MTU, TCP buffer sizes, the route
    // list, the nullable HTTP proxy (a `writeByte` marker), the nullable NAT64
    // prefix, the stacked links (`writeList`), wake-on-LAN and the two
    // captive-portal parcelables.
    //
    // ACOS's network carries no address list (the sandbox has no interface to
    // describe), so every list is an explicit `0`/`-1` count, never a fabricated
    // address. `writeAddress` writes `writeByteArray`, then (for an IPv6
    // address) `writeBoolean` + `writeInt`; the oracle's collapsed loop body is
    // [`FieldOp::Counted`] over exactly those three ops.
    ParcelableSpec {
        class: "android.net.LinkProperties",
        fields: &[
            FieldOp::Str16, // interface name
            FieldOp::Counted(&[FieldOp::Named("android.net.LinkAddress")]), // mLinkAddresses
            FieldOp::Counted(&[FieldOp::ByteArray, FieldOp::Bool, FieldOp::Int]), // mDnses
            // mValidatedPrivateDnses
            FieldOp::Counted(&[FieldOp::ByteArray, FieldOp::Bool, FieldOp::Int]),
            FieldOp::Bool,   // usePrivateDns
            FieldOp::Str16,  // privateDnsServerName
            // mPcscfs
            FieldOp::Counted(&[FieldOp::ByteArray, FieldOp::Bool, FieldOp::Int]),
            FieldOp::Str16,    // domains
            FieldOp::Address, // dhcp server address (writeAddress, IPv4)
            FieldOp::Int,      // mtu
            FieldOp::Str16,    // tcpBufferSizes
            FieldOp::Counted(&[FieldOp::Named("android.net.RouteInfo")]), // mRoutes
            FieldOp::ByteCond(&[FieldOp::Named("android.net.ProxyInfo")]), // mHttpProxy
            FieldOp::Named("android.net.IpPrefix"), // nat64Prefix
            FieldOp::ValueList, // mStackedLinks (`writeList`)
            FieldOp::Bool,      // wakeOnLanSupported
            FieldOp::Named("android.net.CaptivePortalApiUrl"), // mCaptivePortalApiUrl
            FieldOp::Named("android.net.CaptivePortalData"),   // mCaptivePortalData
        ],
        verification: Verification::Full,
    },
    // `NetworkInfo.writeToParcel` (the deprecated query replies).
    ParcelableSpec {
        class: "android.net.NetworkInfo",
        fields: &[
            FieldOp::Int,   // mNetworkType
            FieldOp::Int,   // mSubtype
            FieldOp::Str16, // mTypeName
            FieldOp::Str16, // mSubtypeName
            FieldOp::Str16, // mState.name()
            FieldOp::Str16, // mDetailedState.name()
            FieldOp::Int,   // mIsFailover
            FieldOp::Int,   // mIsAvailable
            FieldOp::Int,   // mIsRoaming
            FieldOp::Str16, // mReason
            FieldOp::Str16, // mExtraInfo
        ],
        verification: Verification::Full,
    },
    // ---- K2/R2: notification channel + group (API-34 image layout) ----------
    //
    // Transcribed from the image's `NotificationChannel(Parcel)` constructor
    // (`classes.dex`), which is the reader every consumer uses. Every nullable
    // member is `writeByte(0|1)` + payload, never `writeInt`.
    ParcelableSpec {
        class: "android.app.NotificationChannel",
        fields: &[
            FieldOp::ByteCond(&[FieldOp::Str16]),  // id
            FieldOp::ByteCond(&[FieldOp::Str16]),  // name
            FieldOp::ByteCond(&[FieldOp::Str16]),  // description
            FieldOp::Int,                          // importance
            FieldOp::Byte,                         // bypassDnd
            FieldOp::Int,                          // lockscreenVisibility
            FieldOp::ByteCond(&[FieldOp::Nested("android.net.Uri")]), // sound
            FieldOp::Byte,                         // lights
            FieldOp::LongArray,                    // vibrationPattern
            FieldOp::Int,                          // userLockedFields
            FieldOp::Byte,                         // userVisibleTaskShown
            FieldOp::Byte,                         // vibrationEnabled
            FieldOp::Byte,                         // showBadge
            FieldOp::Byte,                         // deleted
            FieldOp::ByteCond(&[FieldOp::Str16]),  // group
            FieldOp::Cond(&[FieldOp::Nested("android.media.AudioAttributes")]),
            FieldOp::Int,                          // lightColor
            FieldOp::Bool,                         // blockableSystem
            FieldOp::Int,                          // allowBubbles
            FieldOp::Int,                          // originalImportance
            FieldOp::Str16,                        // parentId
            FieldOp::Str16,                        // conversationId
            FieldOp::Bool,                         // demoted
            FieldOp::Bool,                         // importantConvo
            FieldOp::Long,                         // deletedTime
            FieldOp::Bool,                         // importanceLockedDefaultApp
        ],
        verification: Verification::Prefix,
    },
    ParcelableSpec {
        class: "android.app.NotificationChannelGroup",
        fields: &[
            FieldOp::ByteCond(&[FieldOp::Str16]), // id
            FieldOp::ByteCond(&[FieldOp::Str16]), // name
            FieldOp::ByteCond(&[FieldOp::Str16]), // description
            FieldOp::ByteCond(&[FieldOp::Named(
                "android.content.pm.ParceledListSlice",
            )]),
            FieldOp::Bool, // blocked
            FieldOp::Int,  // userLockedFields
        ],
        verification: Verification::Prefix,
    },
    // ---- K2/R1: the single-user emulated volume ----------------------------
    //
    // `StorageVolume.writeToParcel` (image `classes3.dex`): four string8s, five
    // booleans-as-int, the max file size long, the owner UserHandle written by
    // `writeParcelable` (class name + int), the nullable UUID (int presence +
    // string8), then the fsUuid and state string8s.
    ParcelableSpec {
        class: "android.os.storage.StorageVolume",
        fields: &[
            FieldOp::Str8,  // id
            FieldOp::Str8,  // path
            FieldOp::Str8,  // internalPath
            FieldOp::Str8,  // description
            FieldOp::Int,   // primary
            FieldOp::Int,   // removable
            FieldOp::Int,   // emulated
            FieldOp::Int,   // externallyManaged
            FieldOp::Int,   // allowMassStorage
            FieldOp::Long,  // maxFileSize
            FieldOp::Named("android.os.UserHandle"), // owner
            FieldOp::Cond(&[FieldOp::Str8]), // uuid
            FieldOp::Str8,  // fsUuid
            FieldOp::Str8,  // state
        ],
        verification: Verification::Full,
    },
    ParcelableSpec {
        class: "android.os.UserHandle",
        fields: &[FieldOp::Int],
        // `UserHandle` has two overloads named `writeToParcel`; the DEX scanner
        // follows the static helper, so this one is curated rather than
        // prefix-verified. The wire form is one int (the user id), confirmed
        // against the image's `UserHandle(Parcel)`.
        verification: Verification::Curated,
    },
    ParcelableSpec {
        class: "android.media.AudioAttributes",
        fields: &[
            FieldOp::Int,         // content type
            FieldOp::Int,         // flags
            FieldOp::Int,         // usage
            FieldOp::Int,         // capture preset
            FieldOp::Int,         // ? (image classes2.dex)
            FieldOp::StringArray, // tags (image writes StringArray before the string)
            FieldOp::Str16,       // formatted tags (nullable: -1)
            FieldOp::Int,         // bundle presence
            FieldOp::Bundle,
        ],
        verification: Verification::Prefix,
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
///
/// `android.content.Intent` is not a flat field list (it recurses through
/// `selector`/`originalIntent`, and its `ComponentName` has **no** presence
/// word), so it has a dynamic codec of its own — see [`read_intent`].
pub fn read_payload(class: &str, parcel: &mut Parcel) -> Result<Parcelable> {
    if class == INTENT_CLASS {
        return read_intent(parcel);
    }
    if class == PARCELED_LIST_SLICE {
        return read_parceled_list_slice(parcel);
    }
    let spec = spec_for(class).ok_or_else(|| unsupported(class))?;
    let mut values = Vec::with_capacity(spec.fields.len());
    let trace = std::env::var_os("ACOS_PARCEL_TRACE").is_some();
    for (i, field) in spec.fields.iter().enumerate() {
        let before = parcel.data_position();
        let v = read_field(*field, parcel)?;
        if trace {
            eprintln!("  [{class} {i}] {field:?} {before}..{} = {v:?}", parcel.data_position());
        }
        values.push(v);
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
    if class == INTENT_CLASS {
        return write_intent(parcel, value);
    }
    if class == PARCELED_LIST_SLICE {
        return write_parceled_list_slice(parcel, value);
    }
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
        FieldOp::String8Array => Value::StringArray(parcel.read_string8_array()?),
        FieldOp::StringList => Value::StringList(parcel.read_string16_array()?),
        FieldOp::IntArray => Value::IntArray(parcel.read_int32_array()?),
        FieldOp::LongArray => Value::LongArray(parcel.read_int64_array()?),
        FieldOp::FloatArray => Value::FloatArray(parcel.read_float_array()?),
        FieldOp::ByteArray => Value::ByteArray(parcel.read_byte_array()?),
        FieldOp::Address => {
            let bytes = parcel.read_byte_array()?;
            if bytes.as_ref().is_some_and(|b| b.len() == 16) {
                // IPv6: the writer emits the scope-id marker and value.
                let _has_scope = parcel.read_bool()?;
                let _scope = parcel.read_int32()?;
            }
            Value::ByteArray(bytes)
        }
        FieldOp::Raw => {
            // The native `KeyCharacterMap` payload has no wire length, but it is
            // self-describing (`libs/input/KeyCharacterMap.cpp`): deviceId,
            // hasMap, cstring name, type, overlay, key records and three remap
            // tables. Walk it so a captured `InputDevice` reply is decodable
            // (the replay oracle) and hand the bytes back verbatim.
            let start = parcel.data_position();
            skip_native_key_character_map(parcel)?;
            let end = parcel.data_position();
            let all = parcel.marshall();
            if end > all.len() || start > end {
                return Err(unsupported("native KeyCharacterMap (truncated)"));
            }
            Value::ByteArray(Some(all[start..end].to_vec()))
        }
        FieldOp::SparseArray => {
            let n = parcel.read_int32()?;
            if n < 0 {
                Value::Skipped
            } else if n == 0 {
                // `writeSparseArray(new SparseArray<>())` writes a `0` count
                // (the framework's `DisplayInfo()` initial value). Only the
                // empty array is modelled; see `write_field`.
                Value::List(Some(Vec::new()))
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
                    // `Parcel.writeTypedArray` writes each element with
                    // `writeTypedObject`: a 4-byte presence marker (`0` null,
                    // `1` present) before the payload. The writer below emits
                    // that marker, so the reader must consume it; reading the
                    // payload directly desynchronised every element after the
                    // first and only stayed invisible while all typed arrays in
                    // the replies were empty (count `-1`).
                    if parcel.read_int32()? == 0 {
                        out.push(Value::Parcelable(Parcelable::Null));
                    } else {
                        out.push(Value::Parcelable(read_payload(class, parcel)?));
                    }
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
        FieldOp::ByteCond(fields) => {
            let present = parcel.read_byte()? != 0;
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
        FieldOp::Cutout => read_cutout(parcel)?,
        FieldOp::CountedNested(class) => {
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
                    out.push(Value::Parcelable(read_payload(class, parcel)?));
                }
                Value::List(Some(out))
            }
        }
        FieldOp::Counted(inner) => {
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
                    let mut values = Vec::with_capacity(inner.len());
                    for f in inner {
                        values.push(read_field(*f, parcel)?);
                    }
                    out.push(Value::Cond {
                        present: true,
                        values,
                    });
                }
                Value::List(Some(out))
            }
        }
        FieldOp::ValueList => {
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
                    out.push(Value::Dynamic(read_dynamic(parcel)?));
                }
                Value::List(Some(out))
            }
        }
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

/// Skip one native `KeyCharacterMap` payload (`nativeReadFromParcel` ->
/// `KeyCharacterMap::readFromParcel`), with count bounds so a corrupt parcel is
/// a structured error, never a long loop.
fn skip_native_key_character_map(parcel: &mut Parcel) -> Result<()> {
    const MAX_KEYS: i32 = 8192;
    let _device_id = parcel.read_int32()?;
    let has_map = parcel.read_int32()?;
    if has_map == 0 {
        return Ok(());
    }
    // cstring: raw bytes + NUL, 4-byte padded (`Parcel::readCString` scans).
    let start = parcel.data_position();
    let data = parcel.marshall();
    let nul = data[start..]
        .iter()
        .position(|b| *b == 0)
        .ok_or_else(|| bad("native KeyCharacterMap: unterminated load file name"))?;
    let padded = (nul + 1 + 3) & !3;
    parcel.set_data_position(start + padded);
    let keyboard_type = parcel.read_int32()?;
    if !(0..=6).contains(&keyboard_type) {
        return Err(bad("native KeyCharacterMap: bad keyboard type"));
    }
    let _overlay = parcel.read_int32()?;
    let num_keys = parcel.read_int32()?;
    if !(0..=MAX_KEYS).contains(&num_keys) {
        return Err(bad("native KeyCharacterMap: bad key count"));
    }
    for _ in 0..num_keys {
        let _key_code = parcel.read_int32()?;
        let _label = parcel.read_int32()?;
        let _number = parcel.read_int32()?;
        loop {
            match parcel.read_int32()? {
                0 => break,
                1 => {
                    let _meta = parcel.read_int32()?;
                    let _character = parcel.read_int32()?;
                    let _fallback = parcel.read_int32()?;
                    let _replacement = parcel.read_int32()?;
                }
                _ => return Err(bad("native KeyCharacterMap: bad behaviour marker")),
            }
        }
    }
    for _ in 0..3 {
        let count = parcel.read_int32()?;
        if count < 0 || count as usize > parcel.remaining() / 8 {
            return Err(bad("native KeyCharacterMap: bad remap table count"));
        }
        for _ in 0..count {
            let _from = parcel.read_int32()?;
            let _to = parcel.read_int32()?;
        }
    }
    Ok(())
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
            let refs: Option<Vec<Option<&str>>> =
                arr.map(|v| v.iter().map(|s| s.as_deref()).collect());
            parcel.write_string16_array_nullable(refs.as_deref())?;
        }
        FieldOp::String8Array => {
            let arr = expect_string_array(value)?;
            let refs: Option<Vec<Option<&str>>> =
                arr.map(|v| v.iter().map(|s| s.as_deref()).collect());
            parcel.write_string8_array(refs.as_deref())?;
        }
        FieldOp::StringList => {
            let items = expect_string_list(value)?;
            let refs: Option<Vec<&str>> =
                items.map(|v| v.iter().map(|s| s.as_deref().unwrap_or("")).collect());
            parcel.write_string16_array(refs.as_deref())?;
        }
        FieldOp::IntArray => parcel.write_int32_array(expect_int_array(value)?)?,
        FieldOp::LongArray => parcel.write_int64_array(expect_long_array(value)?)?,
        FieldOp::FloatArray => parcel.write_float_array(expect_float_array(value)?)?,
        FieldOp::ByteArray => parcel.write_byte_array(expect_byte_array(value)?)?,
        FieldOp::Address => match value {
            // A null (and, at 4 bytes, an IPv4 address) is bytes-only.
            Value::ByteArray(None) => parcel.write_byte_array(None)?,
            Value::ByteArray(Some(bytes)) if bytes.len() != 16 => {
                parcel.write_byte_array(Some(bytes))?;
            }
            _ => return Err(unsupported("android.net.Inet6Address scope id")),
        },
        FieldOp::Raw => {
            let bytes = expect_byte_array(value)?.ok_or_else(|| {
                ServiceError::BadReply {
                    interface: "parcelable".into(),
                    method: "raw payload".into(),
                    detail: "null raw bytes".into(),
                }
            })?;
            if bytes.len() % 4 != 0 {
                return Err(ServiceError::BadReply {
                    interface: "parcelable".into(),
                    method: "raw payload".into(),
                    detail: format!("raw payload is {} bytes, not 4-byte aligned", bytes.len()),
                });
            }
            let mut source = Parcel::new();
            source.unmarshall(bytes);
            parcel.append_from(&source, 0, bytes.len())?;
        }
        FieldOp::SparseArray => match value {
            Value::Skipped | Value::List(None) => parcel.write_int32(-1),
            // `DisplayInfo.thermalRefreshRateThrottling` defaults to an empty
            // `SparseArray`, which writes a `0` count, not the null `-1`.
            Value::List(Some(items)) if items.is_empty() => parcel.write_int32(0),
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
                    match expect_parcelable(item)? {
                        Parcelable::Null => parcel.write_int32(0),
                        p => {
                            parcel.write_int32(1);
                            write_payload(parcel, p)?;
                        }
                    }
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
        FieldOp::ByteCond(fields) => {
            let Value::Cond { present, values } = value else {
                return Err(bad(format!("expected byte conditional, got {value:?}")));
            };
            parcel.write_byte(i8::from(*present));
            if *present {
                if values.len() != fields.len() {
                    return Err(bad(format!(
                        "byte conditional expects {} fields, got {}",
                        fields.len(),
                        values.len()
                    )));
                }
                for (field, value) in fields.iter().zip(values) {
                    write_field(*field, parcel, value)?;
                }
            }
            return Ok(());
        }
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
        FieldOp::Cutout => write_cutout(parcel, value)?,
        FieldOp::CountedNested(_class) => match value {
            Value::List(None) | Value::Skipped => parcel.write_int32(-1),
            Value::List(Some(items)) => {
                parcel.write_int32(
                    i32::try_from(items.len()).map_err(|_| bad("counted payloads too large"))?,
                );
                for item in items {
                    write_payload(parcel, expect_parcelable(item)?)?;
                }
            }
            _ => return Err(bad(format!("expected counted payloads, got {value:?}"))),
        },
        FieldOp::Counted(inner) => match value {
            Value::List(None) | Value::Skipped => parcel.write_int32(-1),
            Value::List(Some(items)) => {
                parcel.write_int32(
                    i32::try_from(items.len()).map_err(|_| bad("counted list too large"))?,
                );
                for item in items {
                    let Value::Cond { present, values } = item else {
                        return Err(bad(format!("expected counted element, got {item:?}")));
                    };
                    if !*present || values.len() != inner.len() {
                        return Err(bad(format!(
                            "counted element expects {} fields, got {values:?}",
                            inner.len()
                        )));
                    }
                    for (f, v) in inner.iter().zip(values) {
                        write_field(*f, parcel, v)?;
                    }
                }
            }
            _ => return Err(bad(format!("expected counted list, got {value:?}"))),
        },
        FieldOp::ValueList => match value {
            Value::List(None) | Value::Skipped => parcel.write_int32(-1),
            Value::List(Some(items)) => {
                parcel.write_int32(
                    i32::try_from(items.len()).map_err(|_| bad("value list too large"))?,
                );
                for item in items {
                    let Value::Dynamic(d) = item else {
                        return Err(bad(format!("expected list value, got {item:?}")));
                    };
                    write_dynamic(parcel, d)?;
                }
            }
            _ => return Err(bad(format!("expected value list, got {value:?}"))),
        },
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
fn expect_string_list(v: &Value) -> Result<Option<&Vec<Option<String>>>> {
    match v {
        Value::StringList(a) => Ok(a.as_ref()),
        _ => Err(bad(format!("expected string list, got {v:?}"))),
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
    // `BaseBundle.writeToParcelInner`'s "special case for empty bundles":
    // a bundle with no entries writes a single `0` length and no magic/payload
    // (the framework's own `EMPTY_PARCEL` fast path writes the same `0`). It is
    // an **empty bundle**, not null. K3c hit this: every `ProviderInfo` the
    // framework writes in `publishContentProviders` carries an empty `metaData`.
    if length == 0 {
        return Ok(Some(Bundle::new()));
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
// ---------------------------------------------------------------------------
// android.content.Intent (W35)
// ---------------------------------------------------------------------------

/// The codec key for `android.content.Intent`.
pub const INTENT_CLASS: &str = "android.content.Intent";

/// `ParceledListSlice<T>` has a variable payload (count, element class name,
/// then each element's `writeTypedObject` presence marker + payload) that no
/// flat [`ParcelableSpec`] models. It is decoded into one
/// `Value::List(Some([Value::Parcelable(..)]))` field.
pub const PARCELED_LIST_SLICE: &str = "android.content.pm.ParceledListSlice";

/// The elements of a decoded [`PARCELED_LIST_SLICE`] payload.
#[must_use]
pub fn parceled_list_items(value: &Parcelable) -> Option<&[Value]> {
    let Parcelable::Ordered { class, values } = value else {
        return None;
    };
    if class != PARCELED_LIST_SLICE {
        return None;
    }
    match values.first() {
        Some(Value::List(Some(items))) => Some(items),
        _ => None,
    }
}

/// Build a [`PARCELED_LIST_SLICE`] payload from already-decoded elements.
#[must_use]
pub fn parceled_list_of(items: Vec<Value>) -> Parcelable {
    Parcelable::Ordered {
        class: PARCELED_LIST_SLICE.to_string(),
        values: vec![Value::List(Some(items))],
    }
}

fn read_parceled_list_slice(parcel: &mut Parcel) -> Result<Parcelable> {
    let n = parcel.read_int32()?;
    let mut items = Vec::new();
    if n > 0 {
        let element_class = parcel.read_string16()?.ok_or_else(|| ServiceError::BadRequest {
            interface: PARCELED_LIST_SLICE.into(),
            method: "read".into(),
            detail: "a non-empty ParceledListSlice has no element class name".into(),
        })?;
        let n = n as usize;
        if n > parcel.remaining() {
            return Err(ParcelError::BadLength {
                len: n,
                max: parcel.remaining(),
            }
            .into());
        }
        for _ in 0..n {
            if parcel.read_int32()? == 0 {
                // The rest of the list is fetched from a binder retriever. ACOS
                // only models the inline case; a real device would transact on
                // the retriever, which is not needed for the small channel
                // lists real apps create on install.
                return Err(ServiceError::UnsupportedParcelable {
                    class: format!("{PARCELED_LIST_SLICE} (remote retriever)"),
                });
            }
            items.push(Value::Parcelable(read_payload(&element_class, parcel)?));
        }
    }
    Ok(parceled_list_of(items))
}

fn write_parceled_list_slice(parcel: &mut Parcel, value: &Parcelable) -> Result<()> {
    let items = parceled_list_items(value).ok_or_else(|| bad("malformed ParceledListSlice"))?;
    parcel.write_int32(i32::try_from(items.len()).map_err(|_| bad("list too large"))?);
    if items.is_empty() {
        return Ok(());
    }
    let first = items[0]
        .as_parcelable()
        .ok_or_else(|| bad("ParceledListSlice element is not a parcelable"))?;
    let class = first
        .class()
        .ok_or_else(|| bad("ParceledListSlice cannot carry a null first element"))?;
    parcel.write_string16(class)?;
    for item in items {
        let p = expect_parcelable(item)?;
        match p {
            Parcelable::Null => parcel.write_int32(0),
            Parcelable::Ordered { .. } => {
                parcel.write_int32(1);
                write_payload(parcel, p)?;
            }
            Parcelable::Fd(_) => return Err(bad("ParceledListSlice element is an fd")),
        }
    }
    Ok(())
}
/// `android.content.ComponentName` (the *typed-object* shape: two String16s).
const COMPONENT_CLASS: &str = "android.content.ComponentName";
/// `android.graphics.Rect` (four ints, used for `Intent.mSourceBounds`).
const RECT_CLASS: &str = "android.graphics.Rect";
/// `android.net.Uri` — declared on this image only as the **static**
/// `writeToParcel(Parcel, Uri)`; the body is not decodable from the image, so a
/// non-null `Intent.mData` is a structured refusal, never a guessed body.
const URI_CLASS: &str = "android.net.Uri";
/// `android.content.ClipData` — no codec exists; a present clip is refused.
const CLIP_DATA_CLASS: &str = "android.content.ClipData";

/// Field indices in the [`Parcelable::Ordered`] value [`read_intent`] returns
/// and [`write_intent`] consumes, in `Intent.writeToParcel` order.
pub const INTENT_ACTION: usize = 0;
/// `mData` (`Value::Str(None)` = a null `Uri`).
pub const INTENT_DATA: usize = 1;
/// `mType`.
pub const INTENT_TYPE: usize = 2;
/// `mIdentifier`.
pub const INTENT_IDENTIFIER: usize = 3;
/// `mFlags`.
pub const INTENT_FLAGS: usize = 4;
/// `mPackage`.
pub const INTENT_PACKAGE: usize = 5;
/// `mComponent` (a `ComponentName` parcelable, or `Null`).
pub const INTENT_COMPONENT: usize = 6;
/// `mSourceBounds` (a `Rect` parcelable, or `Null`).
pub const INTENT_SOURCE_BOUNDS: usize = 7;
/// `mCategories` (`StringList`).
pub const INTENT_CATEGORIES: usize = 8;
/// `mSelector` (a nested `Intent`, or `Null`).
pub const INTENT_SELECTOR: usize = 9;
/// `mClipData` (`Null` only).
pub const INTENT_CLIP_DATA: usize = 10;
/// `mContentUserHint`.
pub const INTENT_CONTENT_USER_HINT: usize = 11;
/// `mExtras` (`Bundle`).
pub const INTENT_EXTRAS: usize = 12;
/// `mOriginalIntent` (a nested `Intent`, or `Null`).
pub const INTENT_ORIGINAL: usize = 13;
/// Number of fields the codec produces.
pub const INTENT_FIELDS: usize = 14;

/// Deepest `selector`/`originalIntent` nesting accepted. A real Intent nests at
/// most two; the cap keeps a hostile parcel from recursing without bound.
pub const MAX_INTENT_DEPTH: usize = 4;
/// Largest accepted `mCategories` count.
pub const MAX_INTENT_CATEGORIES: i32 = 1024;

/// Write an `android.content.Intent` payload (no `writeTypedObject` marker).
///
/// The order is the image's `Landroid/content/Intent;->writeToParcel` sequence
/// (W32 §3), and the two wire traps are honoured:
///
/// * every Intent-level string is **String8** (`writeString8`);
/// * `ComponentName` here is the *static* writer, so a null component is a
///   **single null String16** (no presence word) and a present one is two
///   String16s — not the typed-object shape.
pub fn write_intent(parcel: &mut Parcel, value: &Parcelable) -> Result<()> {
    let Parcelable::Ordered { class, values } = value else {
        return Err(unsupported(INTENT_CLASS));
    };
    if class != INTENT_CLASS || values.len() != INTENT_FIELDS {
        return Err(ServiceError::BadReply {
            interface: "parcelable".into(),
            method: INTENT_CLASS.into(),
            detail: format!("expected {INTENT_FIELDS} fields, got {}", values.len()),
        });
    }
    parcel.write_string8_opt(expect_str(&values[INTENT_ACTION])?)?;
    match expect_str(&values[INTENT_DATA])? {
        // A null `Uri` is `writeInt(0)`; a non-null one goes through the static
        // `Uri.writeToParcel`, whose body this image does not pin.
        Some(uri) => {
            let _ = uri;
            return Err(unsupported(URI_CLASS));
        }
        None => parcel.write_int32(0),
    }
    parcel.write_string8_opt(expect_str(&values[INTENT_TYPE])?)?;
    parcel.write_string8_opt(expect_str(&values[INTENT_IDENTIFIER])?)?;
    parcel.write_int32(expect_int(&values[INTENT_FLAGS])?);
    parcel.write_string8_opt(expect_str(&values[INTENT_PACKAGE])?)?;
    match expect_parcelable(&values[INTENT_COMPONENT])? {
        Parcelable::Null => parcel.write_string16_opt(None)?,
        Parcelable::Ordered { values, .. } if values.len() == 2 => {
            parcel.write_string16_opt(expect_str(&values[0])?)?;
            parcel.write_string16_opt(expect_str(&values[1])?)?;
        }
        other => {
            return Err(ServiceError::BadReply {
                interface: "parcelable".into(),
                method: INTENT_CLASS.into(),
                detail: format!("component must be two String16s, got {other:?}"),
            })
        }
    };
    match expect_parcelable(&values[INTENT_SOURCE_BOUNDS])? {
        Parcelable::Null => parcel.write_int32(0),
        Parcelable::Ordered { values, .. } if values.len() == 4 => {
            parcel.write_int32(1);
            for v in values {
                parcel.write_int32(expect_int(v)?);
            }
        }
        other => {
            return Err(ServiceError::BadReply {
                interface: "parcelable".into(),
                method: INTENT_CLASS.into(),
                detail: format!("sourceBounds must be four ints, got {other:?}"),
            })
        }
    }
    match &values[INTENT_CATEGORIES] {
        Value::StringList(None) | Value::StringArray(None) => parcel.write_int32(0),
        Value::StringList(Some(items)) | Value::StringArray(Some(items)) => {
            parcel.write_int32(i32::try_from(items.len()).unwrap_or(i32::MAX));
            for item in items {
                parcel.write_string8_opt(item.as_deref())?;
            }
        }
        other => {
            return Err(ServiceError::BadReply {
                interface: "parcelable".into(),
                method: INTENT_CLASS.into(),
                detail: format!("categories must be a String list, got {other:?}"),
            })
        }
    }
    write_nested_intent(parcel, &values[INTENT_SELECTOR])?;
    match expect_parcelable(&values[INTENT_CLIP_DATA])? {
        Parcelable::Null => parcel.write_int32(0),
        _ => return Err(unsupported(CLIP_DATA_CLASS)),
    }
    parcel.write_int32(expect_int(&values[INTENT_CONTENT_USER_HINT])?);
    write_bundle(parcel, expect_bundle(&values[INTENT_EXTRAS])?)?;
    write_nested_intent(parcel, &values[INTENT_ORIGINAL])
}

fn write_nested_intent(parcel: &mut Parcel, value: &Value) -> Result<()> {
    match expect_parcelable(value)? {
        Parcelable::Null => {
            parcel.write_int32(0);
            Ok(())
        }
        nested @ Parcelable::Ordered { .. } => {
            parcel.write_int32(1);
            write_intent(parcel, nested)
        }
        other => Err(ServiceError::BadReply {
            interface: "parcelable".into(),
            method: INTENT_CLASS.into(),
            detail: format!("nested intent must be an Intent or null, got {other:?}"),
        }),
    }
}

/// Read an `android.content.Intent` payload (no `writeTypedObject` marker).
pub fn read_intent(parcel: &mut Parcel) -> Result<Parcelable> {
    read_intent_at(parcel, 0)
}

fn read_intent_at(parcel: &mut Parcel, depth: usize) -> Result<Parcelable> {
    if depth > MAX_INTENT_DEPTH {
        return Err(ServiceError::BadRequest {
            interface: INTENT_CLASS.into(),
            method: "readFromParcel".into(),
            detail: format!("Intent nesting deeper than {MAX_INTENT_DEPTH}"),
        });
    }
    let action = parcel.read_string8()?;
    // `Uri`: `readInt` presence, then the static `Uri.writeToParcel` body. The
    // image's dex does not pin that body, so a non-null `data` Uri is refused
    // with a structured code instead of a guessed string read that would
    // misalign every field after it.
    let data_present = parcel.read_int32()?;
    if data_present != 0 {
        return Err(unsupported(URI_CLASS));
    }
    let type_ = parcel.read_string8()?;
    let identifier = parcel.read_string8()?;
    let flags = parcel.read_int32()?;
    let package = parcel.read_string8()?;
    // `ComponentName.writeToParcel(ComponentName, Parcel)` (static): a null
    // component is one null String16 — **no presence word**.
    let component = match parcel.read_string16()? {
        None => Value::Parcelable(Parcelable::Null),
        Some(package) => {
            let class = parcel.read_string16()?;
            Value::Parcelable(Parcelable::ordered(
                COMPONENT_CLASS,
                vec![Value::Str(Some(package)), Value::Str(class)],
            ))
        }
    };
    let source_bounds = if parcel.read_int32()? != 0 {
        let mut values = Vec::with_capacity(4);
        for _ in 0..4 {
            values.push(Value::Int(parcel.read_int32()?));
        }
        Value::Parcelable(Parcelable::ordered(RECT_CLASS, values))
    } else {
        Value::Parcelable(Parcelable::Null)
    };
    let count = parcel.read_int32()?;
    if count > MAX_INTENT_CATEGORIES {
        return Err(ServiceError::BadRequest {
            interface: INTENT_CLASS.into(),
            method: "readFromParcel".into(),
            detail: format!("category count {count} exceeds {MAX_INTENT_CATEGORIES}"),
        });
    }
    let categories = if count <= 0 {
        Value::StringList(None)
    } else {
        let mut items = Vec::with_capacity(count as usize);
        for _ in 0..count {
            items.push(parcel.read_string8()?);
        }
        Value::StringList(Some(items))
    };
    let selector = read_nested_intent(parcel, depth)?;
    // MPROC1: a `writeTypedObject` ClipData marker may legitimately be present.
    // `Instrumentation.execStartActivity` calls `Intent.migrateExtraStreamToClipData`,
    // so the framework itself puts a ClipData on any `ACTION_SEND` text/image
    // intent before the binder call — refusing it made every share unsupported.
    // The payload is consumed field by field (the wire stays aligned for the
    // fields after it) and not retained: no ACOS consumer reads `clipData`, and
    // the SEND text is also in `EXTRA_TEXT`, which the capture records.
    let clip_data = if parcel.read_int32()? != 0 {
        read_clip_data(parcel)?;
        Value::Parcelable(Parcelable::Null)
    } else {
        Value::Parcelable(Parcelable::Null)
    };
    let content_user_hint = parcel.read_int32()?;
    let extras = Value::Bundle(read_bundle(parcel)?);
    let original = read_nested_intent(parcel, depth)?;
    Ok(Parcelable::ordered(
        INTENT_CLASS,
        vec![
            Value::Str(action),
            Value::Str(None),
            Value::Str(type_),
            Value::Str(identifier),
            Value::Int(flags),
            Value::Str(package),
            component,
            source_bounds,
            categories,
            selector,
            clip_data,
            Value::Int(content_user_hint),
            extras,
            original,
        ],
    ))
}

/// Largest accepted `ClipData` item count (a bounded reader, never a guess).
pub const MAX_CLIP_ITEMS: i32 = 256;

/// Consume an `android.content.ClipData` payload (`writeToParcel` order, no
/// marker: the `writeTypedObject` presence word is read by the caller).
///
/// The wire is:
///
/// ```text
/// ClipDescription: CharSequence label, String16[] mimeTypes,
///                  PersistableBundle extras, long timeStamp,
///                  boolean isStyledText, int classificationStatus,
///                  Bundle confidences
/// int iconPresent   (1 = Bitmap follows — never produced for a text/URI clip)
/// int itemCount
///   per item: CharSequence text, String8 htmlText,
///             TypedObject Intent, TypedObject IntentSender,
///             TypedObject Uri, TypedObject ActivityInfo, TypedObject TextLinks
/// ```
///
/// Everything a share/pick actually produces is decoded; the two shapes ACOS
/// cannot decode (an icon `Bitmap`, a non-null `TextLinks`/`ActivityInfo`) are a
/// structured `UNSUPPORTED_PARCELABLE` instead of a misaligned parcel.
pub fn skip_clip_data(parcel: &mut Parcel) -> Result<()> {
    read_clip_data(parcel)
}

fn read_clip_data(parcel: &mut Parcel) -> Result<()> {
    // The shape is part of the structured error: "which ClipData field is not
    // modelled" is what tells a caller whether their share can be captured.
    let unsupported_shape = |what: &str| ServiceError::UnsupportedParcelable {
        class: format!("{CLIP_DATA_CLASS} ({what})"),
    };
    // ClipDescription.writeToParcel: written inline (a direct call, no marker).
    let _label = read_field(FieldOp::CharSequence, parcel)?;
    let mime_count = parcel.read_int32()?;
    if mime_count < 0 {
        return Err(unsupported_shape("null mimeTypes"));
    }
    if mime_count > MAX_CLIP_ITEMS {
        return Err(ServiceError::BadRequest {
            interface: INTENT_CLASS.into(),
            method: "readFromParcel".into(),
            detail: format!("ClipData mime type count {mime_count} exceeds {MAX_CLIP_ITEMS}"),
        });
    }
    for _ in 0..mime_count {
        let _ = parcel.read_string16()?;
    }
    // `PersistableBundle mExtras`: the `writeBundle` shape (`-1` null, `0` the
    // framework's empty fast path, else `[length][magic][count][entries]`).
    let _ = read_bundle(parcel)?;
    let _time_stamp = parcel.read_int64()?;
    let _is_styled = parcel.read_bool()?;
    let _classification_status = parcel.read_int32()?;
    // `confidencesToBundle()`: a real Bundle (`writeBundle` shape).
    let _ = read_bundle(parcel)?;
    if parcel.read_int32()? != 0 {
        return Err(unsupported_shape("icon (a Bitmap payload)"));
    }
    let items = parcel.read_int32()?;
    if items < 0 {
        return Err(unsupported_shape("null item list"));
    }
    if items > MAX_CLIP_ITEMS {
        return Err(ServiceError::BadRequest {
            interface: INTENT_CLASS.into(),
            method: "readFromParcel".into(),
            detail: format!("ClipData item count {items} exceeds {MAX_CLIP_ITEMS}"),
        });
    }
    for _ in 0..items {
        // `TextUtils.writeToParcel(item.mText, dest, flags)`: the same
        // `CharSequence` shape the parcelable codec already decodes.
        let _text = read_field(FieldOp::CharSequence, parcel)?;
        let _html = parcel.read_string8()?;
        // The item's four `writeTypedObject` slots on this image (the share
        // sheet's own shape for `ACTION_SEND` text is "all null"). A non-null
        // payload is a structured refusal, never a guessed skip: the slots are
        // `Uri`/`Intent`/`IntentSender`/`ActivityInfo` for an item that carries
        // a stream, and the capture target records the *text* SEND case.
        for _ in 0..4 {
            if parcel.read_int32()? != 0 {
                return Err(unsupported_shape("non-null item payload"));
            }
        }
    }
    Ok(())
}

fn read_nested_intent(parcel: &mut Parcel, depth: usize) -> Result<Value> {
    if parcel.read_int32()? == 0 {
        return Ok(Value::Parcelable(Parcelable::Null));
    }
    Ok(Value::Parcelable(read_intent_at(parcel, depth + 1)?))
}

pub const BUNDLE_MAGIC: u32 = 0x4c44_4e42;

/// Write a `writeBundle` payload in the real `BaseBundle` layout.
/// Android's `String.hashCode` (`s[0]*31^(n-1) + ...`). `ArrayMap` keeps its
/// `mHashes` sorted by this value, and `Parcel.readArrayMap` appends entries in
/// wire order, so the writer must emit them in ascending-hash order.
fn java_string_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32))
}

/// Write an `android.os.Bundle` whose entries are **parcelables**, in the
/// exact wire form API 34's `Parcel.writeValue` produces for
/// `VAL_PARCELABLE`.
///
/// `write_bundle` goes through [`write_dynamic`], which writes a length word
/// after the `VAL_PARCELABLE` tag. The framework's reader
/// (`Parcel.readValue` → `readParcelableInternal`) does **not** expect that
/// word: `Parcel.writeValue`'s `VAL_PARCELABLE` case is
/// `writeParcelable(v, 0)`, i.e. a class-name string followed by the payload.
/// A `Bundle` whose values are parcelables (the `Messenger` callback messages
/// `ConnectivityService` sends) must therefore be written here.
///
/// Entries are emitted in `ArrayMap` order (ascending Java string hash), the
/// same order [`write_bundle`] uses.
pub fn write_parcelable_bundle(
    parcel: &mut Parcel,
    entries: &[(&str, &Parcelable)],
) -> Result<()> {
    if entries.is_empty() {
        parcel.write_int32(0);
        return Ok(());
    }
    let start = parcel.data_size();
    parcel.write_int32(0);
    parcel.write_int32(BUNDLE_MAGIC as i32);
    let payload_start = parcel.data_size();
    parcel.write_int32(i32::try_from(entries.len()).map_err(|_| bad("bundle too large"))?);
    let mut ordered: Vec<&(&str, &Parcelable)> = entries.iter().collect();
    ordered.sort_by_key(|(key, _)| java_string_hash(key));
    for (key, value) in ordered {
        parcel.write_string16(key)?;
        match value {
            Parcelable::Null => parcel.write_int32(val::NULL),
            Parcelable::Ordered { class, .. } => {
                parcel.write_int32(val::PARCELABLE);
                parcel.write_string16(class)?;
                write_payload(parcel, value)?;
            }
            Parcelable::Fd(_) => {
                return Err(bad("a bundle value cannot be a bare file descriptor"));
            }
        }
    }
    let payload_end = parcel.data_size();
    let len = payload_end - payload_start;
    parcel.set_data_position(start);
    parcel.write_int32(i32::try_from(len).map_err(|_| bad("bundle too large"))?);
    parcel.set_data_position(payload_end);
    Ok(())
}

/// Write an `android.os.Bundle` (the `writeValue`-based form used by
/// `Settings` and framework metadata).
pub fn write_bundle(parcel: &mut Parcel, bundle: Option<&Bundle>) -> Result<()> {
    match bundle {
        None => {
            parcel.write_int32(-1);
            Ok(())
        }
        Some(b) if b.is_empty() => {
            // `BaseBundle.writeToParcelInner`: an empty bundle is one `0` int.
            parcel.write_int32(0);
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

/// Read the length word `Parcel.readValue` consumes for a **length-prefixed**
/// `VAL_*` type, and return `(declared_bytes, body_start)`.
///
/// AOSP android-14.0.0_r75 `base/core/java/android/os/Parcel.java`: the table of
/// length-prefixed types is `isLengthPrefixed` (lines 4908-4926: `VAL_MAP`,
/// `VAL_PARCELABLE`, `VAL_LIST`, `VAL_SPARSEARRAY`, `VAL_PARCELABLEARRAY`,
/// `VAL_OBJECTARRAY`, `VAL_SERIALIZABLE`), `writeValue` writes the tag, a
/// placeholder and then backpatches `end - start` (lines 2458-2481), and
/// `readValue` reads that word and compares it to the bytes the body consumed
/// (lines 4529-4546). A negative count, or one larger than the bytes left in the
/// parcel, is refused before anything is allocated.
fn read_value_length(parcel: &mut Parcel, tag: i32) -> Result<(usize, usize)> {
    let declared = parcel.read_int32()?;
    if declared < 0 {
        return Err(bad(format!("VAL_{tag} body length {declared} is negative")));
    }
    let declared = declared as usize;
    let start = parcel.data_position();
    if declared > parcel.remaining() {
        return Err(bad(format!(
            "VAL_{tag} body length {declared} exceeds the {} bytes left in the parcel",
            parcel.remaining()
        )));
    }
    Ok((declared, start))
}

/// Where AOSP's `readValue` logs `Slog.wtfStack` ("… consumed N bytes, but M
/// expected", Parcel.java:4539-4543) on a length mismatch, ACOS refuses: a
/// desynchronised parcel must never be handed to the next field reader.
fn check_value_length(parcel: &Parcel, declared: usize, start: usize, tag: i32) -> Result<()> {
    let actual = parcel.data_position().saturating_sub(start);
    if actual != declared {
        return Err(bad(format!(
            "VAL_{tag} body consumed {actual} bytes but declared {declared}"
        )));
    }
    Ok(())
}

/// Write a length-prefixed `writeValue` body exactly as `Parcel.writeValue`
/// does (android-14.0.0_r75 `base/core/java/android/os/Parcel.java:2458-2481`):
/// the tag, a backpatched byte count of everything after it, then the body.
fn write_value_length_prefixed(
    parcel: &mut Parcel,
    tag: i32,
    body: impl FnOnce(&mut Parcel) -> Result<()>,
) -> Result<()> {
    parcel.write_int32(tag);
    let length_at = parcel.data_size();
    parcel.write_int32(0);
    let start = parcel.data_size();
    body(parcel)?;
    let end = parcel.data_size();
    parcel.set_data_position(length_at);
    parcel.write_int32(i32::try_from(end - start).map_err(|_| bad("writeValue body too large"))?);
    parcel.set_data_position(end);
    Ok(())
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
            // `VAL_PARCELABLE` is length-prefixed (`isLengthPrefixed`,
            // Parcel.java:4908-4926; the writer is `writeValue` at 2458-2481 +
            // `writeParcelable` at 2688 + case 2594-2595), and the length covers
            // the class name *and* the `writeToParcel` body.
            let (declared, start) = read_value_length(parcel, val::PARCELABLE)?;
            let value = match parcel.read_string16()? {
                // `readParcelableCreatorInternal` returns null for a null class
                // name (Parcel.java:5081-5085) and `readParcelableInternal`
                // turns that into a null Parcelable (5006-5011): a null name is
                // a null value, not a malformed parcel.
                None => D::Null,
                Some(class) => D::Parcelable(read_payload(&class, parcel)?),
            };
            check_value_length(parcel, declared, start, val::PARCELABLE)?;
            value
        }
        val::MAP => {
            // `VAL_MAP` is length-prefixed too (`isLengthPrefixed`,
            // Parcel.java:4908-4926): the length word precedes `writeMap`'s
            // size + alternating key/value `writeValue` payloads
            // (`writeMap`, Parcel.java:2586; `readHashMapInternal` 5432).
            // `Settings$NameValueCache` reads the `LIST_config` reply's `value`
            // key as one of these.
            let (declared, start) = read_value_length(parcel, val::MAP)?;
            let size = parcel.read_int32()?;
            let value = if size < 0 {
                D::Null
            } else {
                // Guest-controlled count: every entry is at least two 4-byte
                // `writeValue` type tags, so never reserve more than the parcel
                // can hold (sandbox §5 gate 1; a hostile `i32::MAX` must not
                // allocate). Truncation is still caught by the reads below.
                let cap = (size as usize).min(parcel.remaining() / 8);
                let mut entries = Vec::with_capacity(cap);
                for _ in 0..size {
                    let k = read_dynamic(parcel)?;
                    let v = read_dynamic(parcel)?;
                    entries.push((k, v));
                }
                D::Map(entries)
            };
            check_value_length(parcel, declared, start, val::MAP)?;
            value
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
            // `VAL_PARCELABLE` is length-prefixed (`isLengthPrefixed`,
            // Parcel.java:4908-4926) and the count covers the class name plus
            // the `writeToParcel` body (`writeValue` 2458-2481, case 2594-2595).
            write_value_length_prefixed(parcel, val::PARCELABLE, |parcel| match p {
                Parcelable::Null => {
                    parcel.write_string16_opt(None)?;
                    Ok(())
                }
                Parcelable::Ordered { class, .. } => {
                    parcel.write_string16(class)?;
                    write_payload(parcel, p)
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
                    )
                }
            })?;
        }
        D::Map(entries) => {
            // `VAL_MAP` is length-prefixed (`isLengthPrefixed`,
            // Parcel.java:4908-4926) and `writeMap` (Parcel.java:2586) is the
            // body: size then alternating `writeValue` pairs. A `HashMap` is
            // written as `VAL_MAP` (it is a `Map`, so `writeValue` never reaches
            // the `Serializable` branch).
            write_value_length_prefixed(parcel, val::MAP, |parcel| {
                parcel.write_int32(
                    i32::try_from(entries.len()).map_err(|_| bad("map too large"))?,
                );
                for (k, v) in entries {
                    write_dynamic(parcel, k)?;
                    write_dynamic(parcel, v)?;
                }
                Ok(())
            })?;
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
// DisplayCutout$ParcelableWrapper.writeCutoutToParcel
// ---------------------------------------------------------------------------

/// The class name ACOS uses for a decoded `writeCutoutToParcel` value.
///
/// The wire form is *not* `DisplayCutout.writeToParcel` (there is none):
/// `ParcelableWrapper.writeCutoutToParcel` writes an `int` variant marker
/// (`-1` null, `0` `NO_CUTOUT`, `1` geometry) and the geometry inline
/// (`frameworks/base/core/java/android/view/DisplayCutout.java:1462-1483`).
pub const DISPLAY_CUTOUT_CLASS: &str = "android.view.DisplayCutout";

/// `DisplayCutout.Bounds.BOUNDS_POSITION_LENGTH`: the framework reads the
/// bounding rects into a preallocated `Rect[4]`.
pub const CUTOUT_BOUNDS_LEN: usize = 4;

/// Read a `writeTypedObject` field (presence marker + bare payload).
fn read_typed_class(parcel: &mut Parcel, class: &str) -> Result<Value> {
    if parcel.read_int32()? == 0 {
        Ok(Value::Parcelable(Parcelable::Null))
    } else {
        Ok(Value::Parcelable(read_payload(class, parcel)?))
    }
}

/// Write a `writeTypedObject` field (presence marker + bare payload).
fn write_typed_value(parcel: &mut Parcel, value: &Value) -> Result<()> {
    match expect_parcelable(value)? {
        Parcelable::Null => {
            parcel.write_int32(0);
            Ok(())
        }
        p @ Parcelable::Ordered { .. } => {
            parcel.write_int32(1);
            write_payload(parcel, p)
        }
        Parcelable::Fd(_) => Err(bad("typed parcelable is an fd")),
    }
}

fn read_cutout(parcel: &mut Parcel) -> Result<Value> {
    let variant = parcel.read_int32()?;
    let mut values = vec![Value::Int(variant)];
    match variant {
        -1 | 0 => {}
        1 => {
            values.push(read_typed_class(parcel, "android.graphics.Rect")?); // safeInsets
            let n = parcel.read_int32()?; // writeTypedArray(mBounds.getRects())
            if n < 0 {
                values.push(Value::List(None));
            } else {
                let n = n as usize;
                // The image's `readCutoutFromParcel` reads into a **fixed**
                // `Rect[4]` via `Parcel.readTypedArray(T[] val, Creator)`, which
                // throws `RuntimeException("bad array lengths")` when the wire
                // count differs. Reject it the same way instead of decoding a
                // shape the framework cannot read.
                if n != CUTOUT_BOUNDS_LEN {
                    return Err(bad(format!(
                        "DisplayCutout bounds: the image reads Rect[{}], the parcel has {n} \
                         (Parcel.readTypedArray throws 'bad array lengths')",
                        CUTOUT_BOUNDS_LEN
                    )));
                }
                if n > parcel.remaining() {
                    return Err(ParcelError::BadLength {
                        len: n,
                        max: parcel.remaining(),
                    }
                    .into());
                }
                let mut rects = Vec::with_capacity(n);
                for _ in 0..n {
                    rects.push(read_typed_class(parcel, "android.graphics.Rect")?);
                }
                values.push(Value::List(Some(rects)));
            }
            values.push(read_typed_class(parcel, "android.graphics.Insets")?); // waterfallInsets
            values.push(Value::Int(parcel.read_int32()?)); // displayWidth
            values.push(Value::Int(parcel.read_int32()?)); // displayHeight
            values.push(Value::Int(parcel.read_int32()?)); // physicalDisplayWidth
            values.push(Value::Int(parcel.read_int32()?)); // physicalDisplayHeight
            values.push(Value::Float(parcel.read_float()?)); // density
            values.push(Value::Str(parcel.read_string16()?)); // cutoutSpec
            values.push(Value::Int(parcel.read_int32()?)); // rotation
            values.push(Value::Float(parcel.read_float()?)); // scale
            values.push(Value::Float(parcel.read_float()?)); // physicalPixelDisplaySizeRatio
        }
        other => return Err(bad(format!("unmodelled DisplayCutout variant {other}"))),
    }
    Ok(Value::Parcelable(Parcelable::ordered(
        DISPLAY_CUTOUT_CLASS,
        values,
    )))
}

fn write_cutout(parcel: &mut Parcel, value: &Value) -> Result<()> {
    let p = expect_parcelable(value)?;
    let fields = p
        .fields()
        .ok_or_else(|| bad("DisplayCutout must be an ordered parcelable"))?;
    let variant = fields
        .first()
        .and_then(Value::as_int)
        .ok_or_else(|| bad("DisplayCutout variant"))?;
    parcel.write_int32(variant);
    if variant == -1 || variant == 0 {
        return Ok(());
    }
    if variant != 1 {
        return Err(bad(format!("unmodelled DisplayCutout variant {variant}")));
    }
    if fields.len() != 13 {
        return Err(bad(format!(
            "DisplayCutout variant 1 expects 13 fields, got {}",
            fields.len()
        )));
    }
    write_typed_value(parcel, &fields[1])?; // safeInsets
    match &fields[2] {
        Value::Skipped | Value::List(None) => parcel.write_int32(-1),
        Value::List(Some(items)) => {
            parcel.write_int32(
                i32::try_from(items.len()).map_err(|_| bad("cutout bounds too large"))?,
            );
            for item in items {
                write_typed_value(parcel, item)?;
            }
        }
        other => return Err(bad(format!("expected cutout bounds, got {other:?}"))),
    }
    write_typed_value(parcel, &fields[3])?; // waterfallInsets
    parcel.write_int32(expect_int(&fields[4])?); // displayWidth
    parcel.write_int32(expect_int(&fields[5])?); // displayHeight
    parcel.write_int32(expect_int(&fields[6])?); // physicalDisplayWidth
    parcel.write_int32(expect_int(&fields[7])?); // physicalDisplayHeight
    parcel.write_float(expect_float(&fields[8])?); // density
    parcel.write_string16_opt(expect_str(&fields[9])?)?; // cutoutSpec
    parcel.write_int32(expect_int(&fields[10])?); // rotation
    parcel.write_float(expect_float(&fields[11])?); // scale
    parcel.write_float(expect_float(&fields[12])?); // physicalPixelDisplaySizeRatio
    Ok(())
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
    /// Scan one class's `writeToParcel` from one DEX directory.
    pub fn write_to_parcel(
        dex_dir: &std::path::Path,
        class: &str,
    ) -> std::result::Result<ScannedWrite, String> {
        scan::DexSet::load_dirs(&[dex_dir.to_path_buf()]).write_to_parcel(class)
    }

    /// Scan one class's `writeToParcel` across every boot-classpath DEX
    /// directory of the image, in classpath order.
    ///
    /// This is the oracle the test uses. Scanning a single `dex/framework`
    /// directory let a whole class of wrong field lists hide: a spec whose
    /// class lives in another boot jar (`android.net.*` in
    /// `framework-connectivity`, `android.*` in the apex jars) was skipped as
    /// "class not found" and stayed `Verification::Full` without ever being
    /// checked.
    pub fn write_to_parcel_boot(class: &str) -> std::result::Result<ScannedWrite, String> {
        boot_scanner()
            .ok_or_else(|| "boot DEX cache unavailable (run from the repo root)".to_string())?
            .write_to_parcel(class)
    }

    /// Scan one class's `writeToParcel` across the given DEX directories, in
    /// the order given.
    pub fn write_to_parcel_in(
        dirs: &[std::path::PathBuf],
        class: &str,
    ) -> std::result::Result<ScannedWrite, String> {
        scan::DexSet::load_dirs(dirs).write_to_parcel(class)
    }
}

/// Every DEX directory of the image's boot classpath, in classpath order.
///
/// `.cache/boot/android-34/dex/<jar-stem>/classes*.dex` is the image's own
/// expansion of each boot-classpath jar. The set is the union of
/// `BOOTCLASSPATH`, `SYSTEMSERVERCLASSPATH` and the other jars the image ships
/// under `dex/`: a spec must not be able to hide in a jar that is on the
/// classpath but not scanned, nor in one the image ships without listing.
#[must_use]
pub fn boot_classpath_dex_dirs() -> Option<Vec<std::path::PathBuf>> {
    let root = crate::workspace_path(".cache/boot/android-34")?;
    let dex_root = root.join("dex");
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    let mut listed: Vec<String> = Vec::new();
    for name in ["BOOTCLASSPATH", "SYSTEMSERVERCLASSPATH"] {
        let Ok(text) = std::fs::read_to_string(root.join(name)) else {
            continue;
        };
        for entry in text.trim().split(':') {
            let stem = entry.trim().trim_end_matches(".jar");
            if stem.is_empty() {
                continue;
            }
            listed.push(stem.to_string());
        }
    }
    for stem in listed {
        let dir = dex_root.join(&stem);
        if dir.is_dir() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    // The image ships more dex directories than the two classpath variables
    // list; include them all so no shipped jar is a blind spot.
    let mut rest: Vec<std::path::PathBuf> = std::fs::read_dir(&dex_root)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !dirs.contains(p))
        .collect();
    rest.sort();
    dirs.extend(rest);
    Some(dirs)
}

/// Lazily loaded, all boot-classpath DEX files of the image.
fn boot_scanner() -> Option<&'static scan::DexSet> {
    static BOOT_SCAN: OnceLock<Option<scan::DexSet>> = OnceLock::new();
    BOOT_SCAN
        .get_or_init(|| {
            let dirs = boot_classpath_dex_dirs()?;
            Some(scan::DexSet::load_dirs(&dirs))
        })
        .as_ref()
}

/// Lazily cached boot DEX, used by [`verify_prefix`] tests.
static BOOT_DEX: OnceLock<Option<Vec<acos_dex::DexFile>>> = OnceLock::new();

/// Load every boot-classpath DEX file of the image if present.
///
/// The `dex/framework`-only version of this helper is what made the
/// `android.net` codecs unverifiable; it now returns the same set
/// [`boot_classpath_dex_dirs`] describes.
#[must_use]
pub fn boot_dex() -> Option<&'static [acos_dex::DexFile]> {
    BOOT_DEX
        .get_or_init(|| {
            let dirs = boot_classpath_dex_dirs()?;
            let mut out = Vec::new();
            for dir in dirs {
                let mut paths: Vec<_> = std::fs::read_dir(&dir)
                    .ok()?
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().map(|x| x == "dex").unwrap_or(false))
                    .collect();
                paths.sort();
                for p in paths {
                    let bytes = std::fs::read(p).ok()?;
                    if let Ok(dex) = acos_dex::DexFile::parse(bytes) {
                        out.push(dex);
                    }
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
        FieldOp::StringArray => call == "writeStringArray",
        // `writeString8Array` and `writeStringArray` are different wire
        // encodings; accepting either for both is how the pm codecs stayed
        // wrong while the DEX oracle passed.
        FieldOp::String8Array => call == "writeString8Array",
        // `Parcel.writeStringList` is a distinct call in a DEX scan, but the
        // bytes it writes are the `writeStringArray` shape (count + String16s).
        FieldOp::StringList => call == "writeStringList",
        FieldOp::IntArray => call == "writeIntArray",
        FieldOp::LongArray => call == "writeLongArray",
        FieldOp::FloatArray => call == "writeFloatArray",
        FieldOp::ByteArray | FieldOp::Address => call == "writeByteArray",
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
        FieldOp::PersistableBundle => {
            // `Parcel.writePersistableBundle`, or a direct nested
            // `PersistableBundle.writeToParcel` call (which the scanner records
            // as `writeParcelable`, e.g. `IntentFilter.mExtras`).
            call == "writePersistableBundle" || call == "writeParcelable"
        }
        FieldOp::TypedList(_) => call == "writeTypedList",
        FieldOp::Interface => call == "writeStrongInterface",
        // The presence marker is a `writeInt`; the branch body is invisible to a
        // linear scanner, so only the marker can be verified.
        FieldOp::Cond(_) => call == "writeInt",
        FieldOp::ByteCond(_) => call == "writeByte",
        FieldOp::CountedTyped(_) => call == "writeInt",
        // `DisplayCutout$ParcelableWrapper.writeCutoutToParcel` is a static
        // helper, which the scanner records as a nested `writeToParcel` call.
        FieldOp::Cutout => call == "writeParcelable",
        FieldOp::CountedNested(_) | FieldOp::Counted(_) => call == "writeInt",
        FieldOp::ValueList => call == "writeList",
        // No Java `writeToParcel` call emits these bytes (the peer's native
        // reader consumes them), so a DEX scan cannot verify them.
        FieldOp::Raw => false,
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
        FieldOp::ByteCond(inner) => {
            if first != "writeByte" {
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
        FieldOp::Address => {
            if first != "writeByteArray" {
                return 0;
            }
            // The collapsed IPv6 arm: `writeBoolean` + `writeInt`.
            let mut n = 1;
            if calls.get(1).map(String::as_str) == Some("writeBoolean")
                && calls.get(2).map(String::as_str) == Some("writeInt")
            {
                n += 2;
            }
            n
        }
        FieldOp::Counted(inner) => {
            if first != "writeInt" {
                return 0;
            }
            // The scanner collapses the element loop to one body pass, so a
            // body that does not follow the count is the empty-list case.
            let mut n = 1;
            for f in inner {
                let c = field_consumes(*f, &calls[n..]);
                if c == 0 {
                    return 1;
                }
                n += c;
            }
            n
        }
        FieldOp::CountedNested(_) => match first.as_str() {
            "writeInt" => match calls.get(1).map(String::as_str) {
                Some("writeParcelable") => 2,
                _ => 1,
            },
            // The scanner collapses the loop and drops the explicit
            // `writeInt(supportedModes.length)` before the body, so the first
            // surviving call is the element's nested `writeToParcel`.
            "writeParcelable" => 1,
            _ => 0,
        },
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
///
/// A scan is *method-level*, not file-level: the parser loads every DEX file of
/// the image's boot classpath once, then finds a class wherever it lives. That
/// is what makes `android.net.NetworkCapabilities` (in `framework-connectivity`)
/// and every apex jar reachable by the same oracle as `dex/framework`.
pub mod scan {
    use acos_dex::decode::{decode, payload_kind, payload_len};
    use acos_dex::{ClassDef, DexFile};

    use super::ScannedWrite;

    /// Every `.dex` file of a set of boot DEX directories, in load order.
    pub struct DexSet {
        files: Vec<DexFile>,
    }

    impl DexSet {
        /// Load the `.dex` files of `dirs`, in the order given.
        #[must_use]
        pub fn load_dirs(dirs: &[std::path::PathBuf]) -> Self {
            let mut files = Vec::new();
            for dir in dirs {
                let mut paths: Vec<_> = std::fs::read_dir(dir)
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().map(|x| x == "dex").unwrap_or(false))
                    .collect();
                paths.sort();
                for path in paths {
                    let Ok(bytes) = std::fs::read(&path) else {
                        continue;
                    };
                    if let Ok(dex) = DexFile::parse(bytes) {
                        files.push(dex);
                    }
                }
            }
            Self { files }
        }

        /// Number of loaded DEX files (evidence for tests and the audit).
        #[must_use]
        pub fn len(&self) -> usize {
            self.files.len()
        }

        /// True when no DEX file could be loaded.
        #[must_use]
        pub fn is_empty(&self) -> bool {
            self.files.is_empty()
        }

        /// The first DEX file that defines `desc`, and its class definition.
        fn find_class(&self, desc: &str) -> Option<(&DexFile, &ClassDef)> {
            for dex in &self.files {
                if let Some(def) = dex.find_class(desc) {
                    return Some((dex, def));
                }
            }
            None
        }

        /// Scan `class`'s `writeToParcel` from this DEX set.
        pub fn write_to_parcel(&self, class: &str) -> Result<ScannedWrite, String> {
            let desc = format!("L{};", class.replace('.', "/"));
            let Some((dex, def)) = self.find_class(&desc) else {
                return Err(format!("class not found: {class}"));
            };
            scan_class(self, dex, def)
        }
    }

    /// Control-flow terminator of a scanned region.
    #[derive(Clone, Copy)]
    enum Term {
        /// The region fell through to its `stop` (or the end of the method).
        Fall,
        /// The region ended in an unconditional `goto` to this target.
        Goto(usize),
    }

    fn scan_class(set: &DexSet, dex: &DexFile, def: &ClassDef) -> Result<ScannedWrite, String> {
        let mut out = ScannedWrite::default();
        let desc = dex
            .class_descriptor(def)
            .map_err(|e| e.to_string())?
            .to_string();
        let mut stack = Vec::new();
        scan_method(set, &desc, "writeToParcel", &mut stack, &mut out)?;
        if out.calls.is_empty() {
            return Err("no writeToParcel".into());
        }
        Ok(out)
    }

    /// Scan `class_desc`'s `method`, appending the `Parcel.write*` calls in
    /// execution order. `invoke-super` and direct nested `writeToParcel` calls
    /// are spliced in so the op list is the full inherited field order.
    fn scan_method(
        set: &DexSet,
        class_desc: &str,
        method: &str,
        stack: &mut Vec<String>,
        out: &mut ScannedWrite,
    ) -> Result<(), String> {
        // A stack, not a "seen" set: the same helper is legitimately called at
        // several positions in one writer (`LinkProperties.writeAddresses` three
        // times, `writeAddress` four times), while a recursive
        // `writeToParcel` chain must still terminate.
        let key = format!("{class_desc}->{method}");
        if stack.iter().any(|k| k == &key) {
            return Ok(());
        }
        stack.push(key);
        let result = scan_method_body(set, class_desc, method, stack, out);
        stack.pop();
        result
    }

    fn scan_method_body(
        set: &DexSet,
        class_desc: &str,
        method: &str,
        stack: &mut Vec<String>,
        out: &mut ScannedWrite,
    ) -> Result<(), String> {
        let Some((dex, def)) = set.find_class(class_desc) else {
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
            scan_region(set, dex, &code.insns, 0, None, stack, out)?;
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
        set: &DexSet,
        dex: &DexFile,
        insns: &[u16],
        mut pc: usize,
        stop: Option<usize>,
        stack: &mut Vec<String>,
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
                let fall_term =
                    scan_region(set, dex, insns, fall, Some(take), stack, &mut fall_out)?;
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
                        let _ = scan_region(
                            set,
                            dex,
                            insns,
                            take,
                            Some(join),
                            stack,
                            &mut take_out,
                        )?;
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
                record_invoke(set, dex, insn.index, insn.opcode.byte(), stack, out)?;
            }
            pc += insn.size as usize;
        }
        Ok(Term::Fall)
    }

    fn record_invoke(
        set: &DexSet,
        dex: &DexFile,
        index: u32,
        opcode: u8,
        stack: &mut Vec<String>,
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
            scan_method(set, m.class, "writeToParcel", stack, out)?;
        } else if m.name == "writeToParcel" || m.name == "writeCutoutToParcel" {
            // A nested Parcelable written through its own `writeToParcel`
            // (not `Parcel.writeParcelable`). Record it as a `writeParcelable`
            // op so a spec's `FieldOp::Nested` can verify the position.
            out.calls.push("writeParcelable".to_string());
            out.delegates.push(m.class.to_string());
        } else if m.name.starts_with("write")
            && m.parameters.first() == Some(&"Landroid/os/Parcel;")
        {
            // A private helper that writes into the Parcel
            // (`LinkProperties.writeAddresses`/`writeAddress`,
            // `NetworkCapabilities.writeParcelableArraySet`). Not inlining it
            // left those fields invisible to the oracle: `LinkProperties`
            // claimed 17 calls when the writer emits 30. Splice the helper's
            // own calls in, exactly like `invoke-super`.
            scan_method(set, m.class, m.name, stack, out)?;
        }
        Ok(())
    }
}
