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
/// `android.content.res.CompatibilityInfo`.
pub const COMPATIBILITY_INFO: &str = "android.content.res.CompatibilityInfo";

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
            Extra::Bool(b) => Value::Bool(*b),
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
        facts.mime_type.as_deref(),
        facts.flags,
        facts.package.as_deref(),
        facts.component.as_ref().map(|(p, c)| (p.as_str(), c.as_str())),
        &facts.categories,
        if bundle.is_empty() { None } else { Some(bundle) },
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
            Value::StringList(Some(
                categories.iter().map(|c| Some(c.clone())).collect(),
            )),
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
/// shouldSendCompatFakeFocus)`.
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
        let list = |items: &[Parcelable]| {
            Value::List(Some(
                items
                    .iter()
                    .cloned()
                    .map(Value::Parcelable)
                    .collect::<Vec<_>>(),
            ))
        };
        Parcelable::ordered(
            LAUNCH_ACTIVITY_ITEM,
            vec![
                Value::TypedObject(self.intent.clone()),   // mIntent
                Value::Int(self.ident),                    // mIdent
                Value::TypedObject(self.info.clone()),     // mInfo
                Value::TypedObject(self.cur_config.clone()), // mCurConfig
                Value::TypedObject(self.override_config.clone()), // mOverrideConfig
                Value::Int(self.device_id),                // mDeviceId
                Value::Str(self.referrer.clone()),         // mReferrer
                Value::Binder(None),                       // mVoiceInteractor
                Value::Int(self.proc_state),               // mProcState
                Value::Bundle(None),                       // mState
                Value::Bundle(None),                       // mPersistentState
                list(&self.pending_results),               // mPendingResults
                list(&self.pending_new_intents),           // mPendingNewIntents
                Value::Bundle(None),                       // mActivityOptions
                Value::Bool(self.is_forward),              // mIsForward
                Value::TypedObject(Parcelable::Null),      // mProfilerInfo
                Value::Binder(self.assist_token),          // mAssistToken
                Value::Binder(None),                       // mActivityClientController
                Value::Binder(self.shareable_activity_token), // mShareableActivityToken
                Value::Bool(self.launched_from_bubble),    // mLaunchedFromBubble
                Value::Binder(self.task_fragment_token),   // mTaskFragmentToken
            ],
        )
    }
}

// ---------------------------------------------------------------------------
// android.app.servertransaction.ClientTransaction
// ---------------------------------------------------------------------------

/// A `ClientTransaction`: one lifecycle request plus its callback items.
///
/// `mIsGlobal` is false and `mIsReady` true — the values ATMS writes for a
/// transaction it sends to one app process (a transaction that is not part of a
/// global ordering group and may execute immediately).
#[must_use]
pub fn client_transaction(
    activity_token: BinderId,
    lifecycle_state_request: Option<Parcelable>,
    callbacks: Vec<Parcelable>,
) -> Parcelable {
    Parcelable::ordered(
        CLIENT_TRANSACTION,
        vec![
            Value::Bool(false), // mIsGlobal
            Value::Binder(Some(activity_token)),
            match lifecycle_state_request {
                Some(item) => Value::NamedParcelable(item),
                None => Value::NamedParcelable(Parcelable::Null),
            },
            Value::Bool(true), // mIsReady
            Value::List(Some(
                callbacks.into_iter().map(Value::Parcelable).collect(),
            )),
        ],
    )
}

// ---------------------------------------------------------------------------
// process configuration / compatibility info
// ---------------------------------------------------------------------------

/// The process `Configuration` a launch carries — the same logical value
/// `bindApplication` delivers (`bind_app::build_config`: locale `en-US`,
/// density and dp bounds from the active display override, portrait,
/// `fontScale` 1.0), in `Configuration.writeToParcel` order.
///
/// `mWindowConfiguration` is the display's bounds (the unfilled dp bounds are
/// the compatibility bounds the framework derives for a full-screen activity).
#[must_use]
pub fn process_configuration() -> Parcelable {
    let (density_dpi, width_dp, height_dp, width_px, height_px) =
        match crate::display::display_override() {
            Some(o) => (
                o.density_dpi,
                o.screen_width_dp(),
                o.screen_height_dp(),
                o.width,
                o.height,
            ),
            None => (420, 411, 842, 1080, 2400),
        };
    let window = Parcelable::ordered(
        "android.app.WindowConfiguration",
        vec![
            rect(0, 0, width_px, height_px), // mBounds
            Value::TypedObject(Parcelable::Null), // mAppBounds
            rect(0, 0, width_px, height_px), // mMaxBounds
            Value::Int(1),                   // mWindowingMode (fullscreen)
            Value::Int(1),                   // mActivityType (standard)
            Value::Int(0),                   // mAlwaysOnTop
            Value::Int(0),                   // mRotation
            Value::Int(1),                   // mDisplayWindowingMode (fullscreen)
            Value::Int(0),                   // mDisplayRotation
        ],
    );
    let locales = Value::TypedObject(Parcelable::ordered(
        "android.os.LocaleList",
        vec![Value::Str8(Some("en-US".to_string()))],
    ));
    Parcelable::ordered(
        CONFIGURATION,
        vec![
            Value::Float(1.0), // fontScale
            Value::Int(0),     // mcc
            Value::Int(0),     // mnc
            locales,           // mLocaleList
            Value::Int(1),     // userSetLocale (setLocales sets it)
            Value::Int(1),     // touchscreen (finger)
            Value::Int(1),     // keyboard (nokeys)
            Value::Int(1),     // keyboardHidden
            Value::Int(1),     // hardKeyboardHidden
            Value::Int(1),     // navigation (nonav)
            Value::Int(2),     // navigationHidden
            Value::Int(1),     // orientation (portrait)
            Value::Int(0),     // screenLayout
            Value::Int(0),     // colorMode
            Value::Int(0x11),  // uiMode (normal + no night mode)
            Value::Int(width_dp),        // screenWidthDp
            Value::Int(height_dp),       // screenHeightDp
            Value::Int(width_dp),        // smallestScreenWidthDp
            Value::Int(density_dpi),     // densityDpi
            Value::Int(0),               // compatScreenWidthDp
            Value::Int(0),               // compatScreenHeightDp
            Value::Int(0),               // compatSmallestScreenWidthDp
            Value::Parcelable(window),   // mWindowConfiguration
            Value::Int(0),               // assetsSeq
            Value::Int(0),               // seq
            Value::Int(0),               // fontWeightAdjustment
            Value::Int(0),               // mGrammaticalGender
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

/// The `CompatibilityInfo` for a non-compat, non-scaling app: no compat flags,
/// the display density, scale 1.0 (`CompatibilityInfo.DEFAULT_COMPATIBILITY_INFO`
/// is what ATMS hands `LaunchActivityItem`).
#[must_use]
pub fn compatibility_info() -> Parcelable {
    let density_dpi = match crate::display::display_override() {
        Some(o) => o.density_dpi,
        None => 420,
    };
    Parcelable::ordered(
        COMPATIBILITY_INFO,
        vec![
            Value::Int(0),        // mCompatibilityFlags
            Value::Int(density_dpi), // applicationDensity
            Value::Float(1.0),    // applicationScale
            Value::Float(1.0),    // applicationInvertedScale
        ],
    )
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

/// Build the `ClientTransaction` of a launch: `LaunchActivityItem` callback and
/// a `ResumeActivityItem` lifecycle request, i.e. exactly what ATMS sends for
/// `ActivityRecord` with `andResume = true`.
#[must_use]
pub fn launch_transaction(
    launch: &LaunchActivity,
    activity_token: BinderId,
    resume: Parcelable,
) -> Parcelable {
    client_transaction(activity_token, Some(resume), vec![launch.to_parcelable()])
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

impl AtmsState {
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
        let Some(launch) = self
            .ams
            .pending_launches
            .iter()
            .find(|l| l.token == token)
        else {
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
        let config = process_configuration();
        let item = LaunchActivity::plain(
            intent(&launch.intent)?,
            activity_info(app, activity),
            config.clone(),
            config,
            Some(launch.intent_package()),
        );
        let resume = resume_item(PROCESS_STATE_TOP, false, false, false);
        encode(&launch_transaction(&item, activity_token, resume))
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
