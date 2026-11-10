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
    /// `writeString` (UTF-16) — used by `TextUtils.writeToParcel` too.
    Str16,
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
            FieldOp::Str16, // label (TextUtils.writeToParcel)
            FieldOp::Int,   // icon
            FieldOp::Int,   // labelRes
            FieldOp::Bundle,
            FieldOp::Int, // showUser
            FieldOp::Int, // showBadge (boolean on the wire)
        ],
        verification: Verification::Full,
    },
    // ApplicationInfo: the leading 22 fields are transcribed from
    // `classes.dex:Landroid/content/pm/ApplicationInfo;->writeToParcel`; the
    // API-34 `Parcel` squashing wrapper (`maybeWriteSquashed`) is modelled as a
    // literal int marker, and the trailing `SparseArray`/`Parcelling` fields
    // are a documented gap (service-layer REPORT §3.6, T-49).
    ParcelableSpec {
        class: "android.content.pm.ApplicationInfo",
        fields: &[
            FieldOp::Int,  // maybeWriteSquashed marker (0 = not squashed)
            FieldOp::Str8, // PackageItemInfo.name
            FieldOp::Str8, // PackageItemInfo.packageName
            FieldOp::Int,  // banner
            FieldOp::Str16, // label
            FieldOp::Int,  // icon
            FieldOp::Int,  // labelRes
            FieldOp::Bundle,
            FieldOp::Int,  // showUser
            FieldOp::Int,  // showBadge
            FieldOp::Str8, // sourceDir
            FieldOp::Str8, // publicSourceDir
            FieldOp::Str8, // dataDir
            FieldOp::Str8, // deviceProtectedDataDir
            FieldOp::Str8, // credentialProtectedDataDir
            FieldOp::Str8, // nativeLibraryDir
            FieldOp::Int,  // uid
            FieldOp::Int,  // flags
            FieldOp::Int,  // privateFlags
            FieldOp::Int,  // targetSdkVersion
            FieldOp::Int,  // minSdkVersion
            FieldOp::Int,  // theme
            FieldOp::Int,  // processName? (see scan)
            FieldOp::Str8, // processName
            FieldOp::Str8, // className
            FieldOp::Str8, // manageSpaceActivityName
            FieldOp::Str8, // backupAgentName
            FieldOp::StringArray, // sharedLibraryFiles
            FieldOp::StringArray, // resourceDirs
            FieldOp::StringArray, // overlayPaths
            FieldOp::Int,  // targetSandboxVersion
            FieldOp::Str8, // permission
            FieldOp::Str8, // taskAffinity
        ],
        verification: Verification::Prefix,
    },
    ParcelableSpec {
        class: "android.content.pm.ComponentInfo",
        fields: &[
            FieldOp::Str8, // PackageItemInfo.name
            FieldOp::Str8, // PackageItemInfo.packageName
            FieldOp::Int,  // banner
            FieldOp::Str16,
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Bundle,
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Str8, // applicationInfo (embedded)
            FieldOp::Str8, // processName
            FieldOp::StringArray,
            FieldOp::Int,
            FieldOp::Int,
        ],
        verification: Verification::Prefix,
    },
    ParcelableSpec {
        class: "android.content.pm.ActivityInfo",
        fields: &[
            // ComponentInfo prefix
            FieldOp::Str8,
            FieldOp::Str8,
            FieldOp::Int,
            FieldOp::Str16,
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Bundle,
            FieldOp::Int,
            FieldOp::Int,
            FieldOp::Str8,
            FieldOp::Str8,
            FieldOp::StringArray,
            FieldOp::Int,
            FieldOp::Int,
            // ActivityInfo
            FieldOp::Int,  // theme
            FieldOp::Int,  // launchMode
            FieldOp::Int,  // flags
            FieldOp::Int,  // screenOrientation
            FieldOp::Int,  // configChanges
            FieldOp::Int,  // softInputMode
            FieldOp::Str8, // parentActivityName
            FieldOp::Int,  // uiOptions
            FieldOp::Int,  // colorMode
        ],
        verification: Verification::Prefix,
    },
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
            FieldOp::Int,         // ApplicationInfo marker
            FieldOp::Str8,        // applicationInfo (curated)
            FieldOp::Int,         // installLocation
            FieldOp::Long,        // firstInstallTime
            FieldOp::Long,        // lastUpdateTime
            FieldOp::IntArray,    // gids
            FieldOp::StringArray, // activities (curated)
            FieldOp::StringArray, // receivers
            FieldOp::StringArray, // services
            FieldOp::StringArray, // providers
            FieldOp::Int,         // requestedPermissions marker
            FieldOp::StringArray, // requestedPermissions
        ],
        verification: Verification::Prefix,
    },
    ParcelableSpec {
        class: "android.content.pm.ProviderInfo",
        fields: &[
            FieldOp::Str8,        // PackageItemInfo.name
            FieldOp::Str8,        // packageName
            FieldOp::Int,         // banner
            FieldOp::Str16,       // label
            FieldOp::Int,         // icon
            FieldOp::Int,         // labelRes
            FieldOp::Bundle,      // metaData
            FieldOp::Str8,        // authority
            FieldOp::Str8,        // readPermission
            FieldOp::Str8,        // writePermission
            FieldOp::Bool,        // exported
            FieldOp::Bool,        // grantUriPermissions
            FieldOp::Int,         // initOrder
            FieldOp::Str8,        // processName
        ],
        verification: Verification::Prefix,
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
        verification: Verification::Prefix,
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
        verification: Verification::Prefix,
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
    ParcelableSpec {
        class: "android.view.InputDevice",
        fields: &[
            FieldOp::Int,   // id
            FieldOp::Int,   // generation
            FieldOp::Int,   // controllerNumber
            FieldOp::Str8,  // name
            FieldOp::Int,   // sources
            FieldOp::Int,   // vendorId
            FieldOp::Int,   // productId
            FieldOp::Int,   // descriptor
            FieldOp::Int,   // keyboardType
            FieldOp::Int,   // hasMotionRangeX
            FieldOp::Int,   // hasMotionRangeY
            FieldOp::Float, // motionRangeX min
            FieldOp::Float, // motionRangeX max
            FieldOp::Float, // motionRangeY min
            FieldOp::Float, // motionRangeY max
        ],
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
            FieldOp::Int,
            FieldOp::Int,
        ],
        verification: Verification::Prefix,
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
        FieldOp::Str8 => Value::Str8(parcel.read_string8()?),
        FieldOp::StringArray => Value::StringArray(parcel.read_string16_array()?),
        FieldOp::IntArray => Value::IntArray(parcel.read_int32_array()?),
        FieldOp::LongArray => Value::LongArray(parcel.read_int64_array()?),
        FieldOp::FloatArray => Value::FloatArray(parcel.read_float_array()?),
        FieldOp::ByteArray => Value::ByteArray(parcel.read_byte_array()?),
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
            let start = parcel.data_size();
            parcel.write_int32(0);
            parcel.write_int32(BUNDLE_MAGIC as i32);
            let payload_start = parcel.data_size();
            parcel.write_int32(i32::try_from(b.len()).map_err(|_| bad("bundle too large"))?);
            for (key, value) in &b.entries {
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
        D::Map(_) => {
            return Err(bad("nested VAL_MAP is not modelled"));
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
    let mut matched = 0;
    for (field, call) in spec.fields.iter().zip(&scanned.calls) {
        if field_matches(*field, call) {
            matched += 1;
        } else {
            break;
        }
    }
    (matched, scanned.calls.len())
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
        FieldOp::Str8 => call == "writeString8",
        FieldOp::StringArray => call == "writeString8Array" || call == "writeStringArray",
        FieldOp::IntArray => call == "writeIntArray",
        FieldOp::LongArray => call == "writeLongArray",
        FieldOp::FloatArray => call == "writeFloatArray",
        FieldOp::ByteArray => call == "writeByteArray",
        FieldOp::Named(_) | FieldOp::IgnoredNamed(_) => call == "writeParcelable",
        FieldOp::Typed(_) | FieldOp::IgnoredTyped(_) => call == "writeTypedObject",
        FieldOp::Bundle => call == "writeBundle",
        FieldOp::Fd => call == "writeFileDescriptor",
        FieldOp::Binder => call == "writeStrongBinder",
        FieldOp::TypedArray(_) => call == "writeTypedArray",
        FieldOp::Nested(_) => call == "writeParcelable",
        FieldOp::ParcelableList(_) => call == "writeParcelableList",
        FieldOp::Dynamic => call == "writeValue",
        // The presence marker is a `writeInt`; the branch body is invisible to a
        // linear scanner, so only the marker can be verified.
        FieldOp::Cond(_) => call == "writeInt",
        FieldOp::Unsupported(_) => false,
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

    fn scan_class(dex: &DexFile, def: &ClassDef) -> Result<ScannedWrite, String> {
        let mut out = ScannedWrite::default();
        let desc = dex
            .class_descriptor(def)
            .map_err(|e| e.to_string())?
            .to_string();
        let mut visited = std::collections::HashSet::new();
        flatten(dex, &desc, &mut visited, &mut out)?;
        if out.calls.is_empty() {
            return Err("no writeToParcel".into());
        }
        Ok(out)
    }

    /// Walk `class_desc`'s `writeToParcel`, splicing `invoke-super` calls so the
    /// op list is the full inherited field order.
    fn flatten(
        dex: &DexFile,
        class_desc: &str,
        visited: &mut std::collections::HashSet<String>,
        out: &mut ScannedWrite,
    ) -> Result<(), String> {
        if !visited.insert(class_desc.to_string()) {
            return Ok(());
        }
        let Some(def) = dex.find_class(class_desc) else {
            return Ok(());
        };
        let cd = dex
            .class_data(def)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "no class data".to_string())?;
        for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
            let Ok(r) = dex.method_ref(m.method_idx) else {
                continue;
            };
            if r.name != "writeToParcel" || m.code_off == 0 {
                continue;
            }
            let code = dex.code_item(m.code_off).map_err(|e| e.to_string())?;
            let mut pc = 0usize;
            let mut guard = 0;
            while pc < code.insns.len() && guard < 20_000 {
                guard += 1;
                if payload_kind(&code.insns, pc).is_some() {
                    pc += payload_len(&code.insns, pc).unwrap_or(1);
                    continue;
                }
                let Ok(insn) = decode(&code.insns, pc) else {
                    pc += 1;
                    continue;
                };
                if insn.opcode.is_invoke() {
                    if let Ok(m) = dex.method_ref(insn.index) {
                        if m.class == "Landroid/os/Parcel;" && m.name.starts_with("write") {
                            if m.name != "writeInterfaceToken"
                                && m.name != "writeNoException"
                                && m.name != "writeStackTrace"
                                && m.name != "writeException"
                            {
                                out.calls.push(m.name.to_string());
                            }
                        } else if m.class == "Landroid/os/Parcel;" && m.name == "maybeWriteSquashed"
                        {
                            out.calls.push("writeInt".to_string());
                        } else if m.class == "Landroid/text/TextUtils;" && m.name == "writeToParcel"
                        {
                            out.calls.push("writeString".to_string());
                        } else if m.name == "writeToParcel" && insn.opcode.byte() == 0x6f {
                            // `invoke-super`: inline the inherited field order.
                            flatten(dex, m.class, visited, out)?;
                        } else if m.name == "writeToParcel" {
                            // A nested Parcelable written through its own
                            // `writeToParcel` (not `Parcel.writeParcelable`).
                            // Record it as a `writeParcelable` op so a spec's
                            // `FieldOp::Nested` can verify the position.
                            out.calls.push("writeParcelable".to_string());
                            out.delegates.push(m.class.to_string());
                        }
                    }
                }
                pc += insn.size as usize;
            }
            return Ok(());
        }
        Ok(())
    }
}
