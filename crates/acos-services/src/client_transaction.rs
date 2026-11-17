//! Host-built launch transactions (ADR-0029 amendment "the host builds launch
//! transactions", MPROC6).
//!
//! On a device `ActivityTaskManagerService` builds an
//! `android.app.servertransaction.ClientTransaction` and hands it to
//! `IApplicationThread.scheduleTransaction`. The transaction carries callback
//! items (`LaunchActivityItem`, `ActivityResultItem`, `NewIntentItem`) and one
//! lifecycle request (`ResumeActivityItem`, `PauseActivityItem`, …) that
//! `TransactionExecutor` runs in order.
//!
//! ACOS's activity manager is Rust, so *it* has to write those bytes: a launch
//! into an already-running app process (and every later launch flavour: extras,
//! results, flags, `singleTop`/`onNewIntent`) is a `ClientTransaction` the host
//! encodes and delivers on the existing transport. No helper inside the app
//! builds launches (`option B` was rejected).
//!
//! The field order and presence of every item below is the **image's** own
//! framework dex (`.cache/boot/android-34/dex/framework`, the parcelable
//! scanner; MPROC6 evidence `results/handoff/MPROC6/`), not the AOSP tag:
//!
//! ```text
//! ClientTransaction.writeToParcel
//!   writeBoolean(mIsGlobal) writeStrongBinder(mActivityToken)
//!   writeParcelable(mLifecycleStateRequest) writeBoolean(mIsReady)
//!   writeParcelableList(mActivityCallbacks)
//! ```
//!
//! The image's `LaunchActivityItem` carries **21** fields and stops at
//! `mTaskFragmentToken`. `initialCallerInfoAccessToken`, `activityWindowInfo`
//! and `sceneTransitionInfo` are *not* in this image (they are Android 15
//! fields; the AOSP source cache is newer there) and are therefore not written.
//!
//! Every item is built as a [`Parcelable`] value and encoded with the existing
//! [`crate::parcelable`] codecs (`Intent`, `Bundle`, `ResultInfo`,
//! `ActivityInfo` from the PMS, `Configuration`), so the bytes come from one
//! wire-format implementation, checked byte-for-byte against real Java by
//! `tests/client_transaction_java.rs`.
//!
//! `CompatibilityInfo` is **not** one of the item's fields on this image (the
//! item stops at `mTaskFragmentToken`); the compatibility information a launch
//! needs travels in `bindApplication`, which already carries it.

use acos_parcel::{BinderId, Parcel};

use crate::ams::{Extra, IntentFacts, PendingLaunch};
use crate::atms::AtmsState;
use crate::error::{Result, ServiceError};
use crate::parcelable;
use crate::pms::{ActivityInfo as PmsActivityInfo, AppInfo};
use crate::value::{Bundle, Parcelable, Value};

/// `android.app.servertransaction.ClientTransaction`.
pub const CLIENT_TRANSACTION: &str = "android.app.servertransaction.ClientTransaction";
/// `android.app.servertransaction.LaunchActivityItem`.
pub const LAUNCH_ACTIVITY_ITEM: &str = "android.app.servertransaction.LaunchActivityItem";
/// `android.app.servertransaction.ResumeActivityItem`.
pub const RESUME_ACTIVITY_ITEM: &str = "android.app.servertransaction.ResumeActivityItem";
/// `android.app.servertransaction.PauseActivityItem`.
pub const PAUSE_ACTIVITY_ITEM: &str = "android.app.servertransaction.PauseActivityItem";
/// `android.app.servertransaction.StopActivityItem`.
pub const STOP_ACTIVITY_ITEM: &str = "android.app.servertransaction.StopActivityItem";
/// `android.app.servertransaction.DestroyActivityItem`.
pub const DESTROY_ACTIVITY_ITEM: &str = "android.app.servertransaction.DestroyActivityItem";
/// `android.app.servertransaction.ActivityResultItem`.
pub const ACTIVITY_RESULT_ITEM: &str = "android.app.servertransaction.ActivityResultItem";
/// `android.app.servertransaction.NewIntentItem`.
pub const NEW_INTENT_ITEM: &str = "android.app.servertransaction.NewIntentItem";
/// `android.app.ResultInfo` (the element of `ActivityResultItem`).
pub const RESULT_INFO: &str = "android.app.ResultInfo";
/// `com.android.internal.content.ReferrerIntent` (the element of
/// `NewIntentItem`, and `LaunchActivityItem`'s pending-new-intents).
pub const REFERRER_INTENT: &str = "com.android.internal.content.ReferrerIntent";
/// `android.content.Intent`.
pub const INTENT: &str = parcelable::INTENT_CLASS;
/// `android.content.res.Configuration`.
pub const CONFIGURATION: &str = "android.content.res.Configuration";

/// `ActivityManager.PROCESS_STATE_TOP` — the process state a launch and a
/// resume carry (`ATMS` reports the top state for the process it launches).
pub const PROCESS_STATE_TOP: i32 = 2;
/// `Intent.mContentUserHint` default (`UserHandle.USER_CURRENT`).
const USER_CURRENT: i32 = -2;
/// `ActivityInfo.FLAG_HARDWARE_ACCELERATED` — the API default for an app with
/// `targetSdk >= 14`.
pub const FLAG_HARDWARE_ACCELERATED: i32 = 0x0000_0200;

/// Encode one parcelable to its marshalled bytes (what goes on the wire).
///
/// # Errors
/// A codec refusal (a value the [`crate::parcelable`] spec cannot represent) is
/// returned as a structured [`ServiceError`], never as guessed bytes.
pub fn encode(value: &Parcelable) -> Result<Vec<u8>> {
    let mut parcel = Parcel::new();
    parcelable::write_payload(&mut parcel, value)?;
    Ok(parcel.marshall())
}

/// Decode a payload back into a [`Parcelable`] (the Rust mirror of
/// `CREATOR.createFromParcel`; used by the tests and the evidence dump).
///
/// # Errors
/// A malformed or unsupported payload is a structured error.
pub fn decode(class: &str, bytes: &[u8]) -> Result<Parcelable> {
    let mut parcel = Parcel::new();
    parcel.unmarshall(bytes);
    let value = parcelable::read_payload(class, &mut parcel)?;
    if parcel.data_position() != bytes.len() {
        return Err(ServiceError::BadRequest {
            interface: "client_transaction".into(),
            method: "decode".into(),
            detail: format!(
                "{class}: {} trailing bytes after the payload",
                bytes.len() - parcel.data_position()
            ),
        });
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// android.content.Intent
// ---------------------------------------------------------------------------

/// The `Intent` payload for an intent the host recorded as [`IntentFacts`].
///
/// `mExtras` is reproduced from the recorded extras (string/int/bool). An extra
/// whose value ACOS recorded only as *present* (MPROC1 keeps no value for other
/// types) is a structured refusal: dropping it silently would launch the
/// activity without data the caller sent.
///
/// # Errors
/// [`ServiceError::UnsupportedParcelable`] for an extra with no recorded value.
pub fn intent(facts: &IntentFacts) -> Result<Parcelable> {
    let mut bundle = Bundle::new();
    for (key, extra) in &facts.extras {
        let value = match extra {
            Extra::String(s) => Value::Str(Some(s.clone())),
            Extra::Int(i) => Value::Int(*i),
            Extra::Short(i) => Value::Int(i32::from(*i)),
            Extra::Bool(b) => Value::Bool(*b),
            Extra::Strings(items) => {
                Value::StringArray(Some(items.iter().cloned().map(Some).collect()))
            }
            Extra::Bytes(bytes) => Value::ByteArray(Some(bytes.clone())),
            Extra::ByteArrays(arrays) => Value::ObjectArray(Some(
                arrays
                    .iter()
                    .cloned()
                    .map(|bytes| Value::ByteArray(Some(bytes)))
                    .collect(),
            )),
            Extra::Parcelable(payload) => Value::Parcelable(payload.clone()),
            Extra::Present => {
                return Err(ServiceError::UnsupportedParcelable {
                    class: format!("intent extra `{key}` (ACOS recorded no value)"),
                })
            }
        };
        bundle.put(key.clone(), value);
    }
    Ok(intent_payload(
        facts.action.as_deref(),
        facts.type_.as_deref(),
        facts.flags,
        facts.package.as_deref(),
        facts
            .component
            .as_ref()
            .map(|(p, c)| (p.as_str(), c.as_str())),
        &facts.categories,
        if bundle.is_empty() {
            None
        } else {
            Some(bundle)
        },
    ))
}

/// A bare `android.content.Intent` payload in `Intent.writeToParcel` order.
///
/// `data` (`mData`) is a `Uri`: the image declares its writer only as the
/// **static** `Uri.writeToParcel(Parcel, Uri)`, whose body its dex does not
/// pin, so a non-null data Uri is refused rather than guessed. `mSelector`,
/// `mClipData` and `mOriginalIntent` are null on the launch path (ATMS sends a
/// plain intent) and are written as the null encodings.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn intent_payload(
    action: Option<&str>,
    mime_type: Option<&str>,
    flags: i32,
    package: Option<&str>,
    component: Option<(&str, &str)>,
    categories: &[String],
    extras: Option<Bundle>,
) -> Parcelable {
    Parcelable::ordered(
        INTENT,
        vec![
            Value::Str8(action.map(str::to_string)),
            Value::Str(None), // mData (Uri): null on the launch path
            Value::Str8(mime_type.map(str::to_string)),
            Value::Str8(None), // mIdentifier
            Value::Int(flags),
            Value::Str8(package.map(str::to_string)),
            match component {
                Some((pkg, class)) => Value::Parcelable(Parcelable::ordered(
                    "android.content.ComponentName",
                    vec![
                        Value::Str(Some(pkg.to_string())),
                        Value::Str(Some(class.to_string())),
                    ],
                )),
                None => Value::Parcelable(Parcelable::Null),
            },
            Value::Parcelable(Parcelable::Null), // mSourceBounds (Rect)
            Value::StringList(Some(categories.iter().map(|c| Some(c.clone())).collect())),
            Value::Parcelable(Parcelable::Null), // mSelector
            Value::Parcelable(Parcelable::Null), // mClipData
            Value::Int(USER_CURRENT),            // mContentUserHint
            Value::Bundle(extras),               // mExtras
            Value::Parcelable(Parcelable::Null), // mOriginalIntent
        ],
    )
}

// ---------------------------------------------------------------------------
// android.app.ResultInfo / com.android.internal.content.ReferrerIntent
// ---------------------------------------------------------------------------

/// One `android.app.ResultInfo` (the element of `ActivityResultItem`).
///
/// The image's `writeToParcel` writes `mResultWho`, `mRequestCode`,
/// `mResultCode`, the `mData` presence word and the `Intent`; it has **no**
/// caller-token field (the AOSP source cache is newer there).
#[must_use]
pub fn result_info(
    result_who: Option<&str>,
    request_code: i32,
    result_code: i32,
    data: Option<Parcelable>,
) -> Parcelable {
    match data {
        Some(intent) => Parcelable::ordered(
            RESULT_INFO,
            vec![
                Value::Str(result_who.map(str::to_string)),
                Value::Int(request_code),
                Value::Int(result_code),
                Value::Cond {
                    present: true,
                    values: vec![Value::Parcelable(intent)],
                },
            ],
        ),
        None => Parcelable::ordered(
            RESULT_INFO,
            vec![
                Value::Str(result_who.map(str::to_string)),
                Value::Int(request_code),
                Value::Int(result_code),
                Value::Cond {
                    present: false,
                    values: Vec::new(),
                },
            ],
        ),
    }
}

/// One `com.android.internal.content.ReferrerIntent`: an `Intent` payload plus
/// the referring package (`ReferrerIntent.writeToParcel` is those two writes —
/// the `Splice` superclass contributes no marker).
#[must_use]
pub fn referrer_intent(intent: Parcelable, referrer: Option<&str>) -> Parcelable {
    Parcelable::ordered(
        REFERRER_INTENT,
        vec![
            Value::Parcelable(intent),
            Value::Str(referrer.map(str::to_string)),
        ],
    )
}

// ---------------------------------------------------------------------------
// lifecycle items
// ---------------------------------------------------------------------------

/// `ResumeActivityItem.obtain(procState, updateProcState, isForward,
/// shouldSendCompatFakeFocus)` — the four wire fields.
///
/// The image's own factory is `obtain(procState, isForward,
/// shouldSendCompatFakeFocus)`, which always sets `mUpdateProcState = true`
/// (dexdump of the image's `ResumeActivityItem.obtain(IZZ)`); `ATMS` uses that
/// form, so a host-built resume passes `update_proc_state = true`.
#[must_use]
pub fn resume_item(
    proc_state: i32,
    update_proc_state: bool,
    is_forward: bool,
    should_send_compat_fake_focus: bool,
) -> Parcelable {
    Parcelable::ordered(
        RESUME_ACTIVITY_ITEM,
        vec![
            Value::Int(proc_state),
            Value::Bool(update_proc_state),
            Value::Bool(is_forward),
            Value::Bool(should_send_compat_fake_focus),
        ],
    )
}

/// `PauseActivityItem.obtain(finished, userLeaving, configChanges, dontReport,
/// autoEnteringPip)`.
#[must_use]
pub fn pause_item(
    finished: bool,
    user_leaving: bool,
    config_changes: i32,
    dont_report: bool,
    auto_entering_pip: bool,
) -> Parcelable {
    Parcelable::ordered(
        PAUSE_ACTIVITY_ITEM,
        vec![
            Value::Bool(finished),
            Value::Bool(user_leaving),
            Value::Int(config_changes),
            Value::Bool(dont_report),
            Value::Bool(auto_entering_pip),
        ],
    )
}

/// `StopActivityItem.obtain(configChanges)`.
#[must_use]
pub fn stop_item(config_changes: i32) -> Parcelable {
    Parcelable::ordered(STOP_ACTIVITY_ITEM, vec![Value::Int(config_changes)])
}

/// `DestroyActivityItem.obtain(finished, configChanges)`.
#[must_use]
pub fn destroy_item(finished: bool, config_changes: i32) -> Parcelable {
    Parcelable::ordered(
        DESTROY_ACTIVITY_ITEM,
        vec![Value::Bool(finished), Value::Int(config_changes)],
    )
}

// ---------------------------------------------------------------------------
// callback items
// ---------------------------------------------------------------------------

/// `ActivityResultItem.obtain(resultInfoList)`.
#[must_use]
pub fn activity_result_item(results: Vec<Parcelable>) -> Parcelable {
    Parcelable::ordered(
        ACTIVITY_RESULT_ITEM,
        vec![Value::List(Some(
            results.into_iter().map(Value::Parcelable).collect(),
        ))],
    )
}

/// `NewIntentItem.obtain(intents, resume)` — `intents` are
/// [`referrer_intent`] values.
#[must_use]
pub fn new_intent_item(intents: Vec<Parcelable>, resume: bool) -> Parcelable {
    Parcelable::ordered(
        NEW_INTENT_ITEM,
        vec![
            Value::Bool(resume),
            Value::List(Some(intents.into_iter().map(Value::Parcelable).collect())),
        ],
    )
}

/// The logical `LaunchActivityItem` the host fills for one launch.
///
/// Field names follow the image's `LaunchActivityItem`; the absent fields are
/// the null encodings the framework writes for them on this path
/// (`mVoiceInteractor`, `mProfilerInfo`, `mActivityClientController` are null;
/// `mState`/`mPersistentState`/`mActivityOptions` are null).
#[derive(Debug, Clone)]
pub struct LaunchActivity {
    /// `mIntent`.
    pub intent: Parcelable,
    /// `mIdent` (`0` on the launch path; ATMS uses it for relaunch identity).
    pub ident: i32,
    /// `mInfo` (the resolved `ActivityInfo`).
    pub info: Parcelable,
    /// `mCurConfig` (the process configuration).
    pub cur_config: Parcelable,
    /// `mOverrideConfig` (the activity's override; equal to `mCurConfig` when
    /// there is none).
    pub override_config: Parcelable,
    /// `mDeviceId` (`Context.DEVICE_ID_DEFAULT` = 0).
    pub device_id: i32,
    /// `mReferrer` (the launching package).
    pub referrer: Option<String>,
    /// `mProcState` (the launching process's state; `PROCESS_STATE_TOP`).
    pub proc_state: i32,
    /// `mPendingResults` (`ActivityResultItem` results owed to this launch).
    pub pending_results: Vec<Parcelable>,
    /// `mPendingNewIntents` (`singleTop`/`singleTask` intents).
    pub pending_new_intents: Vec<Parcelable>,
    /// `mIsForward`.
    pub is_forward: bool,
    /// `mAssistToken`.
    pub assist_token: Option<BinderId>,
    /// `mShareableActivityToken`.
    pub shareable_activity_token: Option<BinderId>,
    /// `mLaunchedFromBubble`.
    pub launched_from_bubble: bool,
    /// `mTaskFragmentToken`.
    pub task_fragment_token: Option<BinderId>,
}

impl LaunchActivity {
    /// The `LaunchActivityItem` payload for a plain launch: no pending results
    /// or new intents, no assist/shareable/task-fragment token, no bubble.
    #[must_use]
    pub fn plain(
        intent: Parcelable,
        info: Parcelable,
        config: Parcelable,
        override_config: Parcelable,
        referrer: Option<String>,
    ) -> Self {
        Self {
            intent,
            ident: 0,
            info,
            cur_config: config,
            override_config,
            device_id: 0,
            referrer,
            proc_state: PROCESS_STATE_TOP,
            pending_results: Vec::new(),
            pending_new_intents: Vec::new(),
            is_forward: false,
            assist_token: None,
            shareable_activity_token: None,
            launched_from_bubble: false,
            task_fragment_token: None,
        }
    }

    /// The `LaunchActivityItem` payload, in the image's write order.
    #[must_use]
    pub fn to_parcelable(&self) -> Parcelable {
        // A null list (not an empty one) is what `LaunchActivityItem.obtain`
        // stores when there is nothing pending, and `writeTypedList` writes the
        // `-1` for it.
        let list = |items: &[Parcelable]| {
            if items.is_empty() {
                Value::List(None)
            } else {
                Value::List(Some(
                    items
                        .iter()
                        .cloned()
                        .map(Value::Parcelable)
                        .collect::<Vec<_>>(),
                ))
            }
        };
        Parcelable::ordered(
            LAUNCH_ACTIVITY_ITEM,
            vec![
                Value::TypedObject(self.intent.clone()),          // mIntent
                Value::Int(self.ident),                           // mIdent
                Value::TypedObject(self.info.clone()),            // mInfo
                Value::TypedObject(self.cur_config.clone()),      // mCurConfig
                Value::TypedObject(self.override_config.clone()), // mOverrideConfig
                Value::Int(self.device_id),                       // mDeviceId
                Value::Str(self.referrer.clone()),                // mReferrer
                Value::Binder(None),                              // mVoiceInteractor
                Value::Int(self.proc_state),                      // mProcState
                Value::Bundle(None),                              // mState
                Value::Bundle(None),                              // mPersistentState
                list(&self.pending_results),                      // mPendingResults
                list(&self.pending_new_intents),                  // mPendingNewIntents
                Value::Bundle(None),                              // mActivityOptions
                Value::Bool(self.is_forward),                     // mIsForward
                Value::TypedObject(Parcelable::Null),             // mProfilerInfo
                Value::Binder(self.assist_token),                 // mAssistToken
                Value::Binder(None),                              // mActivityClientController
                Value::Binder(self.shareable_activity_token),     // mShareableActivityToken
                Value::Bool(self.launched_from_bubble),           // mLaunchedFromBubble
                Value::Binder(self.task_fragment_token),          // mTaskFragmentToken
            ],
        )
    }
}

// ---------------------------------------------------------------------------
// android.app.servertransaction.ClientTransaction
// ---------------------------------------------------------------------------

/// A `ClientTransaction`: one lifecycle request plus its callback items.
///
/// The image's `writeToParcel` writes **two presence flags**, not the
/// `mIsGlobal`/`mIsReady` pair an older AOSP source suggests (this build has no
/// such fields — see the `ClientTransaction` spec in [`crate::parcelable`]):
///
/// ```text
///   writeBoolean(mActivityToken != null); writeStrongBinder(mActivityToken)
///   writeParcelable(mLifecycleStateRequest)
///   writeBoolean(mActivityCallbacks != null)
///   if (non-null) writeParcelableList(mActivityCallbacks)
/// ```
///
/// So a **lifecycle-only** transaction (no callbacks — what ATMS sends for
/// `PauseActivityItem`/`StopActivityItem`/`ResumeActivityItem`) writes the
/// `false` and then *nothing at all*: not a null list and not an empty one.
/// That is the answer to the "empty list or null" question MPROC5 left open, and
/// the real-Java differential pins it (`tests/client_transaction_java.rs`,
/// case `client_transaction_lifecycle_only`).
#[must_use]
pub fn client_transaction(
    activity_token: Option<BinderId>,
    lifecycle_state_request: Option<Parcelable>,
    callbacks: Vec<Parcelable>,
) -> Parcelable {
    Parcelable::ordered(
        CLIENT_TRANSACTION,
        vec![
            match activity_token {
                Some(token) if token != BinderId::NULL => Value::Cond {
                    present: true,
                    values: vec![Value::Binder(Some(token))],
                },
                _ => Value::Cond {
                    present: false,
                    values: Vec::new(),
                },
            },
            match lifecycle_state_request {
                Some(item) => Value::NamedParcelable(item),
                None => Value::NamedParcelable(Parcelable::Null),
            },
            if callbacks.is_empty() {
                Value::Cond {
                    present: false,
                    values: Vec::new(),
                }
            } else {
                Value::Cond {
                    present: true,
                    values: vec![Value::List(Some(
                        callbacks.into_iter().map(Value::Parcelable).collect(),
                    ))],
                }
            },
        ],
    )
}

// ---------------------------------------------------------------------------
// process configuration / compatibility info
// ---------------------------------------------------------------------------

/// The process `Configuration` a launch carries — the same logical value
/// `bindApplication` delivers (`bind_app::build_config`: locale `en-US`, the
/// active display's density and dp bounds, `fontScale` 1.0, portrait), in
/// `Configuration.writeToParcel` order.
///
/// Every other field keeps the framework's own default, because
/// `bind_app::build_config` sets nothing else: the values below are read off the
/// real `new Configuration()` through the differential
/// (`tests/client_transaction_java.rs`), not guessed — e.g. `setLocales` leaves
/// `userSetLocale` false but sets the `LAYOUTDIR_LTR` bits in `screenLayout`,
/// `mWindowConfiguration` stays empty, `mRotation`/`mDisplayRotation` are
/// `ROTATION_UNDEFINED` (`-1`) and `fontWeightAdjustment` is
/// `FONT_WEIGHT_ADJUSTMENT_UNDEFINED` (`i32::MAX`).
///
/// The launch `Configuration` must stay byte-identical to the one
/// `bindApplication` sends: the guest merges it into the activity's
/// `ActivityClientRecord` and an unknown difference shows up as a spurious
/// `onConfigurationChanged`. (Carrying the display bounds instead is a change
/// for *both* paths, not a launch-only one.)
#[must_use]
pub fn process_configuration() -> Parcelable {
    let (density_dpi, width_dp, height_dp) = match crate::display::display_override() {
        Some(o) => (o.density_dpi, o.screen_width_dp(), o.screen_height_dp()),
        None => (420, 411, 842),
    };
    // `new Configuration()` + `setLocales` — the framework's empty
    // `WindowConfiguration` (all-zero bounds, no app bounds, undefined modes).
    let window = Parcelable::ordered(
        "android.app.WindowConfiguration",
        vec![
            rect(0, 0, 0, 0),                     // mBounds
            Value::TypedObject(Parcelable::Null), // mAppBounds
            rect(0, 0, 0, 0),                     // mMaxBounds
            Value::Int(0),                        // mWindowingMode
            Value::Int(0),                        // mActivityType
            Value::Int(0),                        // mAlwaysOnTop
            Value::Int(-1),                       // mRotation (undefined)
            Value::Int(0),                        // mDisplayWindowingMode
            Value::Int(-1),                       // mDisplayRotation (undefined)
        ],
    );
    let locales = Value::TypedObject(Parcelable::ordered(
        "android.os.LocaleList",
        vec![Value::Str8(Some("en-US".to_string()))],
    ));
    Parcelable::ordered(
        CONFIGURATION,
        vec![
            Value::Float(1.0),         // fontScale
            Value::Int(0),             // mcc
            Value::Int(0),             // mnc
            locales,                   // mLocaleList
            Value::Int(0),             // userSetLocale (setLocales does not set it)
            Value::Int(0),             // touchscreen (undefined: the ctor default)
            Value::Int(0),             // keyboard (undefined)
            Value::Int(0),             // keyboardHidden (undefined)
            Value::Int(0),             // hardKeyboardHidden (undefined)
            Value::Int(0),             // navigation (undefined)
            Value::Int(0),             // navigationHidden (undefined)
            Value::Int(1),             // orientation (PORTRAIT — bind_app sets it)
            Value::Int(0x40),          // screenLayout: LAYOUTDIR_LTR, set by setLocales
            Value::Int(0),             // colorMode
            Value::Int(0),             // uiMode
            Value::Int(width_dp),      // screenWidthDp
            Value::Int(height_dp),     // screenHeightDp
            Value::Int(width_dp),      // smallestScreenWidthDp
            Value::Int(density_dpi),   // densityDpi
            Value::Int(0),             // compatScreenWidthDp
            Value::Int(0),             // compatScreenHeightDp
            Value::Int(0),             // compatSmallestScreenWidthDp
            Value::Parcelable(window), // mWindowConfiguration
            Value::Int(0),             // assetsSeq
            Value::Int(0),             // seq
            Value::Int(i32::MAX),      // fontWeightAdjustment (undefined)
            Value::Int(0),             // mGrammaticalGender
        ],
    )
}

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Value {
    Value::Parcelable(Parcelable::ordered(
        "android.graphics.Rect",
        vec![
            Value::Int(left),
            Value::Int(top),
            Value::Int(right),
            Value::Int(bottom),
        ],
    ))
}

// ---------------------------------------------------------------------------
// the launch transaction, and its delivery to a running process
// ---------------------------------------------------------------------------

/// The `ActivityInfo` parcelable for a launch, from the PMS's `AppInfo`.
///
/// `ActivityInfo` embeds its `ApplicationInfo`, so the PMS's own codec is the
/// only correct place to build it.
#[must_use]
pub fn activity_info(app: &AppInfo, activity: &PmsActivityInfo) -> Parcelable {
    activity.to_parcelable(app)
}

/// The `ActivityInfo` a launch item carries on the ACOS guest.
///
/// `ActivityInfo.FLAG_HARDWARE_ACCELERATED` is the API's default for
/// `targetSdk >= 14`, but the ACOS ART guest cannot initialise the framework's
/// hardware renderer: `ActivityThread.handleResumeActivity` →
/// `ViewRootImpl.setView` → `ThreadedRenderer.create` →
/// `HardwareRenderer.ProcessInitializer.initDisplayInfo`, and the guest's
/// `RenderThread` then aborts (SIGABRT; measured MPROC7, proc-b: two
/// `ACOSCRASH` records from the RenderThread). MPROC4's guest-marshalled launch
/// always carried `flags = 0` for exactly that reason; the host encoder must
/// state the same capability or a host-built launch would claim a renderer the
/// session does not have. Clearing the one bit (nothing else) keeps every other
/// manifest field intact.
///
/// Giving the guest a real GL/EGL render path is a separate track (a
/// compositor/engine capability, not an activity-manager decision).
#[must_use]
pub fn launched_activity_info(app: &AppInfo, activity: &PmsActivityInfo) -> Parcelable {
    let mut activity = activity.clone();
    activity.flags &= !FLAG_HARDWARE_ACCELERATED;
    activity_info(app, &activity)
}

/// Build the `ClientTransaction` of a launch: `LaunchActivityItem` callback and
/// a `ResumeActivityItem` lifecycle request, i.e. exactly what ATMS sends for
/// `ActivityRecord` with `andResume = true`.
#[must_use]
pub fn launch_transaction(
    launch: &LaunchActivity,
    activity_token: BinderId,
    resume: Parcelable,
) -> Parcelable {
    client_transaction(
        Some(activity_token),
        Some(resume),
        vec![launch.to_parcelable()],
    )
}

/// A `scheduleTransaction` delivery: the method to call on the process's
/// `IApplicationThread`, the marshalled `ClientTransaction`, and the process
/// tag it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleTransaction {
    /// The app process tag (`AtmsState::processes`), i.e. *which* app thread.
    pub process_tag: u32,
    /// The `IApplicationThread` method (`scheduleTransaction`).
    pub method: &'static str,
    /// The marshalled `android.app.servertransaction.ClientTransaction`.
    pub parcel: Vec<u8>,
}

/// The outcome of turning a recorded launch into a delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchDelivery {
    /// The launch is encoded and ready to deliver to this process tag.
    Ready(ScheduleTransaction),
    /// No `startActivity` is pending for this token (already delivered or
    /// never recorded).
    NoPendingLaunch,
    /// The launch's package has no attached app process yet; the launcher owns
    /// process creation (`ProcessSpawnRequest`).
    ProcessNotRunning,
}

/// The `IApplicationThread.scheduleTransaction` request parcel for one
/// `ClientTransaction` (MPROC7).
///
/// The peer delivers a host-built transaction exactly like a `PendingDelivery`:
/// an `F_TXN` whose payload is the AIDL request (`interface token` + the
/// `ClientTransaction` payload). Building it through the decoded
/// `android.app.IApplicationThread` table means the bytes are the same the
/// delivery transport already produces — no second encoder, no second shape.
///
/// # Errors
/// A structured error when the AIDL table has no `scheduleTransaction` or the
/// transaction cannot be encoded.
pub fn schedule_transaction_request(transaction: &Parcelable) -> Result<Parcel> {
    let info = acos_aidl::lookup_by_name("android.app.IApplicationThread", "scheduleTransaction")
        .ok_or_else(|| ServiceError::UnsupportedAndroidApi {
        interface: "android.app.IApplicationThread".into(),
        code: 0,
        method: "scheduleTransaction".into(),
    })?;
    crate::codec::build_request(
        "android.app.IApplicationThread",
        &info,
        &[Value::TypedObject(transaction.clone())],
    )
}

impl AtmsState {
    /// MPROC7: encode the launch recorded for `package` for its **running**
    /// process, adopting a host-minted activity token.
    ///
    /// This is the one launch path the ADR asks for: the host builds
    /// `ClientTransaction{LaunchActivityItem + ResumeActivityItem}` from the
    /// image's dex shape (MPROC6's encoder) and the process receives it as
    /// `IApplicationThread.scheduleTransaction`, exactly what `system_server`
    /// sends. It serves
    ///
    /// * `startActivity` into a process that is already running (the next
    ///   activity of the same package, or a second activity of another one), and
    /// * the first launch of a freshly spawned secondary process, whose guest no
    ///   longer marshals its own launch.
    ///
    /// `token` is the host-minted activity token (its id carries the process
    /// tag, see [`crate::host::ServiceHost::register_activity_token`]). The
    /// placeholder token `startActivity` recorded is rewritten to it before the
    /// transaction is encoded, so the guest's `activityResumed(token)` finds the
    /// record and the result routing (`result_to`) keeps naming the caller.
    ///
    /// Returns `None` when no launch is pending for `package` (already
    /// delivered, or never recorded).
    ///
    /// # Errors
    /// A structured error when the recorded intent cannot be encoded.
    pub fn take_launch_for_process(
        &mut self,
        package: &str,
        app: &AppInfo,
        activity: &PmsActivityInfo,
        token: BinderId,
    ) -> Result<Option<Parcelable>> {
        let Some(launch) = self.ams.pending_launch_for(package).cloned() else {
            return Ok(None);
        };
        let transaction = self.encode_launch_parcelable(&launch, app, activity, token)?;
        // Only after a successful encode: the record is now the host's launch,
        // not work the runtime still owes.
        self.ams.take_launch(launch.token);
        self.ams.bind_launch_token(launch.token, token.0);
        self.ensure_activity(token.0, &launch.class);
        if launch.result_to != 0 && launch.result_to != token.0 {
            self.note_result_route(token.0, launch.result_to, launch.request_code);
        }
        Ok(Some(transaction))
    }

    /// MPROC7: encode the next `onNewIntent` owed to process `tag` as the
    /// `ClientTransaction` `system_server` sends for `FLAG_ACTIVITY_SINGLE_TOP`.
    ///
    /// The image's `NewIntentItem.obtain(List, boolean resume)`: one
    /// `ReferrerIntent` in the list and `resume = true`, which is what
    /// `TransactionExecutor` uses to resume the reused activity after
    /// `onNewIntent` (no separate lifecycle request is needed).
    ///
    /// Returns `None` when the process has no `onNewIntent` pending.
    pub fn take_new_intent_for_tag(&mut self, tag: u32) -> Option<Parcelable> {
        let (token, facts) = self.ams.take_pending_new_intent(tag)?;
        let intent = match intent(&facts) {
            Ok(intent) => intent,
            Err(err) => {
                self.log_event(
                    "ActivityManagerNewIntent",
                    acos_protocol::LogLevel::Warn,
                    format!("tag={tag} token={token:#x} NewIntentItem not encoded: {err}"),
                );
                return None;
            }
        };
        let item = new_intent_item(vec![referrer_intent(intent, None)], true);
        self.log_event(
            "ActivityManagerNewIntent",
            acos_protocol::LogLevel::Debug,
            format!("tag={tag} token={token:#x} NewIntentItem(resume=true)"),
        );
        Some(client_transaction(Some(BinderId(token)), None, vec![item]))
    }

    /// MPROC7: record that the activity `token` waits for the result of the
    /// activity `caller` starts (`startActivityForResult`).
    pub fn note_result_route(&mut self, token: u64, result_to: u64, request_code: i32) {
        let Some(index) = self.activities.iter().position(|a| a.token == token) else {
            return;
        };
        self.activities[index].result_to = result_to;
        self.activities[index].request_code = request_code;
    }

    /// Encode the `scheduleTransaction` delivery for a recorded launch.
    ///
    /// This is the narrow host-side call site (MPROC6): it reads the
    /// `PendingLaunch` `AmsState` recorded for `token`, finds the app process
    /// whose package owns the activity, and returns the bytes of the
    /// `ClientTransaction` the process's `IApplicationThread.scheduleTransaction`
    /// takes — a `LaunchActivityItem` + `ResumeActivityItem`, built the way
    /// `system_server` builds it.
    ///
    /// `app`/`activity` are the PMS's `ApplicationInfo` and the resolved
    /// `ActivityInfo` (ATMS resolves the component before it records the
    /// launch), and `activity_token` is the activity-token binder the record
    /// owns — the same binder the guest reports back in `activityResumed`.
    /// Routing the delivery and the lifecycle that follows it are not part of
    /// this function.
    ///
    /// # Errors
    /// A structured error when the recorded intent cannot be encoded (an extra
    /// with no recorded value, or a value the codec refuses).
    pub fn launch_delivery(
        &self,
        token: u64,
        app: &AppInfo,
        activity: &PmsActivityInfo,
        activity_token: BinderId,
    ) -> Result<LaunchDelivery> {
        let Some(launch) = self.ams.pending_launches.iter().find(|l| l.token == token) else {
            return Ok(LaunchDelivery::NoPendingLaunch);
        };
        // The process that owns the activity is the one registered for its
        // package (MPROC4): a launch is delivered to it, never to another tag.
        let Some(process) = self
            .processes
            .iter()
            .find(|p| p.package_name == launch.package && p.attached && p.tag != u32::MAX)
        else {
            return Ok(LaunchDelivery::ProcessNotRunning);
        };
        let transaction = self.encode_launch(launch, app, activity, activity_token)?;
        Ok(LaunchDelivery::Ready(ScheduleTransaction {
            process_tag: process.tag,
            method: "scheduleTransaction",
            parcel: transaction,
        }))
    }

    /// Encode one `PendingLaunch` as a `ClientTransaction` (the bytes of the
    /// `LaunchActivityItem` + `ResumeActivityItem` schedule).
    ///
    /// # Errors
    /// As [`Self::launch_delivery`].
    pub fn encode_launch(
        &self,
        launch: &PendingLaunch,
        app: &AppInfo,
        activity: &PmsActivityInfo,
        activity_token: BinderId,
    ) -> Result<Vec<u8>> {
        encode(&self.encode_launch_parcelable(launch, app, activity, activity_token)?)
    }

    /// The `ClientTransaction` parcelable of a recorded launch (the item tree
    /// MPROC6 pins against real Java, without the IPC parcel envelope).
    ///
    /// # Errors
    /// As [`Self::encode_launch`].
    pub fn encode_launch_parcelable(
        &self,
        launch: &PendingLaunch,
        app: &AppInfo,
        activity: &PmsActivityInfo,
        activity_token: BinderId,
    ) -> Result<Parcelable> {
        let config = process_configuration();
        let item = LaunchActivity::plain(
            intent(&launch.intent)?,
            launched_activity_info(app, activity),
            config.clone(),
            config,
            Some(launch.intent_package()),
        );
        // `ResumeActivityItem.obtain(PROCESS_STATE_TOP, false, false)`: the
        // image's factory re-evaluates the process state.
        let resume = resume_item(PROCESS_STATE_TOP, true, false, false);
        Ok(launch_transaction(&item, activity_token, resume))
    }
}

impl PendingLaunch {
    /// The package the launch came from (the intent's referrer): the explicit
    /// component's package, else the recorded `Intent` package, else the
    /// package that owns the activity (an internal launch has no referrer).
    #[must_use]
    pub fn intent_package(&self) -> String {
        self.intent
            .component
            .as_ref()
            .map(|(p, _)| p.clone())
            .or_else(|| self.intent.package.clone())
            .unwrap_or_else(|| self.package.clone())
    }
}
