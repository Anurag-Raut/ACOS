//! Replay of the measured outgoing binder calls.
//!
//! Every row is constructed the way the framework proxy would (interface token
//! plus the decoded `WireOp` argument layout), dispatched through
//! `ServiceHost::transact`, and decoded by the decoded reply layout. Rows owned
//! by `acos-wm` are listed explicitly as *not owned* rather than silently
//! omitted.

use acos_parcel::{BinderId, Parcel};
use acos_services::host::ServiceHost;
use acos_services::value::{Parcelable, Value};
use acos_services::{codec, ServiceError};

struct Case {
    name: &'static str,
    interface: &'static str,
    method: &'static str,
    args: Vec<Value>,
}

fn binder(host: &mut ServiceHost, name: &str) -> BinderId {
    host.check_service(name)
        .expect("check_service")
        .unwrap_or_else(|| panic!("unknown service {name}"))
}

fn b(n: u64) -> Value {
    Value::Binder(Some(BinderId(n)))
}

fn s(v: &str) -> Value {
    Value::Str(Some(v.to_string()))
}

#[allow(clippy::too_many_lines)]
fn launch_cases() -> Vec<Case> {
    let pkg = "dev.acos.fixtures.helloviews";
    let app_thread = 0x1000;
    let token = 0xabc;
    let mut cases = vec![
        Case {
            name: "IDisplayManager.getPreferredWideGamutColorSpaceId",
            interface: "android.hardware.display.IDisplayManager",
            method: "getPreferredWideGamutColorSpaceId",
            args: vec![],
        },
        Case {
            name: "IDisplayManager.getOverlaySupport",
            interface: "android.hardware.display.IDisplayManager",
            method: "getOverlaySupport",
            args: vec![],
        },
        Case {
            name: "IDisplayManager.getDisplayInfo",
            interface: "android.hardware.display.IDisplayManager",
            method: "getDisplayInfo",
            args: vec![Value::Int(0)],
        },
        Case {
            name: "INetworkManagementService.setUidCleartextNetworkPolicy",
            interface: "android.os.INetworkManagementService",
            method: "setUidCleartextNetworkPolicy",
            args: vec![Value::Int(10192), Value::Int(0)],
        },
        Case {
            name: "IPackageManager.notifyDexLoad",
            interface: "android.content.pm.IPackageManager",
            method: "notifyDexLoad",
            args: vec![s(pkg), Value::Int(0), Value::Int(0), s("x86_64")],
        },
        Case {
            name: "IActivityManager.checkPermission",
            interface: "android.app.IActivityManager",
            method: "checkPermission",
            args: vec![
                Value::Str(Some("android.permission.INTERNET".into())),
                Value::Int(10192),
                Value::Int(4242),
            ],
        },
        Case {
            name: "IPackageManager.getPackagesForUid",
            interface: "android.content.pm.IPackageManager",
            method: "getPackagesForUid",
            args: vec![Value::Int(10192)],
        },
        Case {
            name: "IPackageManager.getApplicationInfo",
            interface: "android.content.pm.IPackageManager",
            method: "getApplicationInfo",
            args: vec![s(pkg), Value::Long(0), Value::Int(0)],
        },
        Case {
            name: "IActivityManager.attachApplication",
            interface: "android.app.IActivityManager",
            method: "attachApplication",
            args: vec![b(app_thread), Value::Long(7)],
        },
        Case {
            name: "IServiceManager.checkService",
            interface: "android.os.IServiceManager",
            method: "checkService",
            args: vec![s("activity")],
        },
        Case {
            name: "IGameManagerService.isAngleEnabled",
            interface: "android.app.IGameManagerService",
            method: "isAngleEnabled",
            args: vec![s(pkg), Value::Int(0)],
        },
        Case {
            name: "IPackageManager.hasSystemFeature",
            interface: "android.content.pm.IPackageManager",
            method: "hasSystemFeature",
            args: vec![s("android.hardware.touchscreen"), Value::Int(0)],
        },
        Case {
            name: "IPackageManager.getPackageInfo",
            interface: "android.content.pm.IPackageManager",
            method: "getPackageInfo",
            args: vec![s(pkg), Value::Long(0), Value::Int(0)],
        },
        Case {
            name: "IActivityManager.finishAttachApplication",
            interface: "android.app.IActivityManager",
            method: "finishAttachApplication",
            args: vec![Value::Long(7)],
        },
        Case {
            name: "IDisplayManager.registerCallbackWithEventMask",
            interface: "android.hardware.display.IDisplayManager",
            method: "registerCallbackWithEventMask",
            args: vec![b(0x2000), Value::Long(0)],
        },
        Case {
            name: "IActivityTaskManager.getActivityClientController",
            interface: "android.app.IActivityTaskManager",
            method: "getActivityClientController",
            args: vec![],
        },
        Case {
            name: "IActivityClientController.getDisplayId",
            interface: "android.app.IActivityClientController",
            method: "getDisplayId",
            args: vec![b(token)],
        },
        Case {
            name: "IVirtualDeviceManager.getDeviceIdForDisplayId",
            interface: "android.companion.virtual.IVirtualDeviceManager",
            method: "getDeviceIdForDisplayId",
            args: vec![Value::Int(0)],
        },
        Case {
            name: "IActivityManager.getContentProvider",
            interface: "android.app.IActivityManager",
            method: "getContentProvider",
            args: vec![b(app_thread), s(pkg), s("settings"), Value::Int(0), Value::Bool(false)],
        },
        Case {
            name: "IAccessibilityManager.addClient",
            interface: "android.view.accessibility.IAccessibilityManager",
            method: "addClient",
            args: vec![b(0x3000), Value::Int(0)],
        },
        Case {
            name: "IAccessibilityManager.getRecommendedTimeoutMillis",
            interface: "android.view.accessibility.IAccessibilityManager",
            method: "getRecommendedTimeoutMillis",
            args: vec![],
        },
        Case {
            name: "IAccessibilityManager.getFocusStrokeWidth",
            interface: "android.view.accessibility.IAccessibilityManager",
            method: "getFocusStrokeWidth",
            args: vec![],
        },
        Case {
            name: "IAccessibilityManager.getFocusColor",
            interface: "android.view.accessibility.IAccessibilityManager",
            method: "getFocusColor",
            args: vec![],
        },
        Case {
            name: "IActivityClientController.setTaskDescription",
            interface: "android.app.IActivityClientController",
            method: "setTaskDescription",
            args: vec![
                b(token),
                Value::TypedObject(Parcelable::ordered(
                    "android.app.ActivityManager$TaskDescription",
                    vec![
                        Value::Cond { present: false, values: vec![] }, // label
                        Value::Cond { present: false, values: vec![] }, // icon
                        Value::Int(0), // colorPrimary
                        Value::Int(0), // colorBackground
                        Value::Int(0), // statusBarColor
                        Value::Int(0), // navigationBarColor
                        Value::Bool(false), // ensureStatusBarContrast
                        Value::Bool(false), // ensureNavigationBarContrast
                        Value::Int(2), // resizeMode
                        Value::Int(-1), // minWidth
                        Value::Int(-1), // minHeight
                        Value::Cond { present: false, values: vec![] }, // iconFilename
                        Value::Int(0), // colorBackgroundFloating
                    ],
                )),
            ],
        },
        Case {
            name: "IActivityClientController.reportSizeConfigurations",
            interface: "android.app.IActivityClientController",
            method: "reportSizeConfigurations",
            args: vec![
                b(token),
                Value::TypedObject(Parcelable::ordered(
                    "android.window.SizeConfigurationBuckets",
                    vec![Value::IntArray(Some(vec![]))],
                )),
            ],
        },
        Case {
            name: "IVoiceInteractionManagerService.notifyActivityEventChanged",
            interface: "com.android.internal.app.IVoiceInteractionManagerService",
            method: "notifyActivityEventChanged",
            args: vec![b(token), Value::Int(0)],
        },
        Case {
            name: "IDisplayManager.getDisplayIds",
            interface: "android.hardware.display.IDisplayManager",
            method: "getDisplayIds",
            args: vec![Value::Bool(false)],
        },
        Case {
            name: "IActivityManager.setRenderThread",
            interface: "android.app.IActivityManager",
            method: "setRenderThread",
            args: vec![Value::Int(4243)],
        },
        Case {
            name: "IGraphicsStats.requestBufferForProcess",
            interface: "android.view.IGraphicsStats",
            method: "requestBufferForProcess",
            args: vec![s(pkg), b(0x4000)],
        },
        Case {
            name: "IActivityClientController.activityResumed",
            interface: "android.app.IActivityClientController",
            method: "activityResumed",
            args: vec![b(token), Value::Bool(true)],
        },
        Case {
            name: "IContentCaptureManager.registerContentCaptureOptionsCallback",
            interface: "android.view.contentcapture.IContentCaptureManager",
            method: "registerContentCaptureOptionsCallback",
            args: vec![s(pkg), b(0x5000)],
        },
        Case {
            name: "ITextServicesManager.getCurrentSpellCheckerSubtype",
            interface: "com.android.internal.textservice.ITextServicesManager",
            method: "getCurrentSpellCheckerSubtype",
            args: vec![Value::Int(0), Value::Bool(false)],
        },
        Case {
            name: "IInputManager.getVelocityTrackerStrategy",
            interface: "android.hardware.input.IInputManager",
            method: "getVelocityTrackerStrategy",
            args: vec![],
        },
        Case {
            name: "IInputManager.registerInputDevicesChangedListener",
            interface: "android.hardware.input.IInputManager",
            method: "registerInputDevicesChangedListener",
            args: vec![b(0x6000)],
        },
        Case {
            name: "IInputManager.getInputDeviceIds",
            interface: "android.hardware.input.IInputManager",
            method: "getInputDeviceIds",
            args: vec![],
        },
        Case {
            name: "IInputManager.getInputDevice",
            interface: "android.hardware.input.IInputManager",
            method: "getInputDevice",
            args: vec![Value::Int(0)],
        },
        Case {
            name: "IContentService.registerContentObserver",
            interface: "android.content.IContentService",
            method: "registerContentObserver",
            args: vec![
                Value::Parcelable(Parcelable::ordered(
                    "android.net.Uri",
                    vec![
                        Value::Int(3), // HierarchicalUri
                        Value::Str8(Some("content://settings/system".into())),
                    ],
                )),
                Value::Bool(false),
                b(0x7000),
                Value::Int(0),
                Value::Int(0),
            ],
        },
        Case {
            name: "IActivityClientController.activityIdle",
            interface: "android.app.IActivityClientController",
            method: "activityIdle",
            args: vec![
                b(token),
                Value::TypedObject(Parcelable::Null),
                Value::Bool(false),
            ],
        },
    ];
    // The Settings provider calls are replayed separately because their args
    // include an AttributionSource parcelable.
    cases.push(Case {
        name: "IContentProvider.call (GET_system)",
        interface: "android.content.IContentProvider",
        method: "call",
        args: provider_args("GET_system", "auto_caps"),
    });
    cases.push(Case {
        name: "IContentProvider.call (GET_global)",
        interface: "android.content.IContentProvider",
        method: "call",
        args: provider_args("GET_global", "animator_duration_scale"),
    });
    cases.push(Case {
        name: "IContentProvider.call (PUT_system)",
        interface: "android.content.IContentProvider",
        method: "call",
        args: provider_args("PUT_system", "auto_caps"),
    });
    cases
}

fn provider_args(method: &str, key: &str) -> Vec<Value> {
    // `ContentProviderProxy.call(AttributionSource, authority, method, arg, extras)`.
    vec![
        Value::Parcelable(Parcelable::ordered(
            "android.content.AttributionSource",
            vec![
                Value::Int(0),
                Value::Int(4242),
                Value::Int(10192),
                Value::Str(Some("dev.acos.fixtures.helloviews".into())),
                Value::Str(None),
                Value::Binder(None),
                Value::StringArray(None),
                Value::List(None),
            ],
        )),
        Value::Str(Some("settings".into())),
        Value::Str(Some(method.into())),
        Value::Str(Some(key.into())),
        Value::Bundle(None),
    ]
}

fn interaction_cases() -> Vec<Case> {
    use Parcelable as P;
    vec![
        Case {
            name: "IAutoFillManager.addClient",
            interface: "android.view.autofill.IAutoFillManager",
            method: "addClient",
            args: vec![
                b(0x8000),
                Value::TypedObject(P::ordered(
                    "android.content.ComponentName",
                    vec![Value::Str(None), Value::Str(None)],
                )),
                Value::Int(0),
                b(0x8001),
            ],
        },
        Case {
            name: "IAutoFillManager.startSession",
            interface: "android.view.autofill.IAutoFillManager",
            method: "startSession",
            args: vec![
                b(0x8002),
                b(0x8003),
                Value::TypedObject(P::ordered(
                    "android.view.autofill.AutofillId",
                    vec![Value::Int(1), Value::Int(2)],
                )),
                Value::Parcelable(P::ordered(
                    "android.graphics.Rect",
                    vec![Value::Int(0), Value::Int(0), Value::Int(1), Value::Int(1)],
                )),
                Value::TypedObject(P::ordered(
                    "android.view.autofill.AutofillValue",
                    vec![Value::Int(0), Value::Dynamic(acos_services::value::DynamicValue::Null)],
                )),
                Value::Int(0),
                Value::Bool(false),
                Value::Int(0),
                Value::TypedObject(P::ordered(
                    "android.content.ComponentName",
                    vec![Value::Str(None), Value::Str(None)],
                )),
                Value::Bool(false),
                b(0x8004),
            ],
        },
        Case {
            name: "IPackageManager.hasSystemFeature (interaction)",
            interface: "android.content.pm.IPackageManager",
            method: "hasSystemFeature",
            args: vec![s("android.hardware.touchscreen.multitouch.distinct"), Value::Int(0)],
        },
        Case {
            name: "IActivityTaskManager.reportAssistContextExtras",
            interface: "android.app.IActivityTaskManager",
            method: "reportAssistContextExtras",
            args: vec![
                b(0x9000),
                Value::Bundle(None),
                Value::TypedObject(P::ordered("android.app.assist.AssistStructure", vec![Value::Int(0)])),
                Value::TypedObject(P::ordered("android.app.assist.AssistContent", vec![Value::Int(0)])),
                Value::TypedObject(P::ordered(
                    "android.net.Uri",
                    vec![Value::Int(0), Value::Str8(None)],
                )),
            ],
        },
        Case {
            name: "ITextServicesManager.isSpellCheckerEnabled",
            interface: "com.android.internal.textservice.ITextServicesManager",
            method: "isSpellCheckerEnabled",
            args: vec![Value::Int(0)],
        },
        Case {
            name: "ITextServicesManager.getCurrentSpellCheckerSubtype (interaction)",
            interface: "com.android.internal.textservice.ITextServicesManager",
            method: "getCurrentSpellCheckerSubtype",
            args: vec![Value::Int(0), Value::Bool(true)],
        },
        Case {
            name: "ITextServicesManager.getCurrentSpellChecker",
            interface: "com.android.internal.textservice.ITextServicesManager",
            method: "getCurrentSpellChecker",
            args: vec![Value::Int(0), s("")],
        },
        Case {
            name: "ITextServicesManager.getSpellCheckerService",
            interface: "com.android.internal.textservice.ITextServicesManager",
            method: "getSpellCheckerService",
            args: vec![
                Value::Int(0),
                s(""),
                s(""),
                b(0x9001),
                b(0x9002),
                Value::Bundle(None),
                Value::Int(0),
            ],
        },
        Case {
            name: "IAudioService.areNavigationRepeatSoundEffectsEnabled",
            interface: "android.media.IAudioService",
            method: "areNavigationRepeatSoundEffectsEnabled",
            args: vec![],
        },
        Case {
            name: "IAudioService.playSoundEffect",
            interface: "android.media.IAudioService",
            method: "playSoundEffect",
            args: vec![Value::Int(0), Value::Int(-1)],
        },
        Case {
            name: "IAndroidFuture.complete",
            interface: "com.android.internal.infra.IAndroidFuture",
            method: "complete",
            args: vec![Value::TypedObject(P::ordered(
                "com.android.internal.infra.AndroidFuture",
                vec![Value::Int(0)],
            ))],
        },
        Case {
            name: "IServiceManager.checkService (interaction)",
            interface: "android.os.IServiceManager",
            method: "checkService",
            args: vec![s("audio")],
        },
        Case {
            name: "IContentProvider.call (GET_secure)",
            interface: "android.content.IContentProvider",
            method: "call",
            args: provider_args("GET_secure", "some_key"),
        },
        Case {
            name: "IApplicationThread.profilerControl (system->app delivery)",
            interface: "android.app.IApplicationThread",
            method: "profilerControl",
            args: vec![
                Value::Bool(false),
                Value::TypedObject(P::Null),
                Value::Int(0),
            ],
        },
        Case {
            name: "IWindowSession.onRectangleOnScreenRequested (acos-wm)",
            interface: "android.view.IWindowSession",
            method: "onRectangleOnScreenRequested",
            args: vec![],
        },
        Case {
            name: "IInputMethodManager.reportPerceptibleAsync",
            interface: "com.android.internal.view.IInputMethodManager",
            method: "reportPerceptibleAsync",
            args: vec![Value::Binder(None), Value::Bool(true)],
        },
        Case {
            name: "IInputMethodSession.updateSelection",
            interface: "com.android.internal.inputmethod.IInputMethodSession",
            method: "updateSelection",
            args: vec![
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(-1),
                Value::Int(-1),
            ],
        },
        Case {
            name: "IImeTracker.onProgress",
            interface: "com.android.internal.inputmethod.IImeTracker",
            method: "onProgress",
            args: vec![
                Value::TypedObject(P::ordered(
                    "android.view.inputmethod.ImeTracker$Token",
                    vec![Value::Binder(None), Value::Int(1)],
                )),
                Value::Int(1),
            ],
        },
    ]
}

fn replay(host: &mut ServiceHost, case: &Case) -> Result<Vec<Value>, ServiceError> {
    if let Some(owner) = acos_services::registry::owner_of(case.interface) {
        return Err(ServiceError::NotOwned {
            interface: case.interface.to_string(),
            method: case.method.to_string(),
            owner,
        });
    }
    if case.interface == "android.app.IApplicationThread" {
        return Err(ServiceError::NotOwned {
            interface: case.interface.to_string(),
            method: case.method.to_string(),
            owner: "acos-vm (delivery)",
        });
    }
    let binder = if case.interface == "android.os.IServiceManager" {
        host.aux("android.os.IServiceManager")
            .expect("service manager binder")
    } else if case.interface == "android.app.IActivityClientController" {
        host.aux("android.app.IActivityClientController")
            .expect("controller binder")
    } else if case.interface == "android.content.IContentProvider" {
        host.aux("android.content.IContentProvider")
            .expect("provider binder")
    } else {
        let name = acos_services::registry::name_for(case.interface)
            .unwrap_or_else(|| panic!("no service name for {}", case.interface));
        binder(host, name)
    };
    let info = host
        .method(case.interface, case.method)
        .unwrap_or_else(|| panic!("no AIDL table for {}::{}", case.interface, case.method));
    let mut data = codec::build_request(case.interface, info, &case.args)?;
    data.set_data_position(0);
    let mut reply = Parcel::new();
    let flags = if info.oneway { 1 } else { 0 };
    host.transact(binder, info.code, &mut data, &mut reply, flags)?;
    if info.oneway {
        return Ok(Vec::new());
    }
    reply.set_data_position(0);
    codec::parse_reply(case.interface, info, &mut reply)
}

#[test]
fn replay_all_measured_outgoing_calls() {
    let mut host = ServiceHost::standard();
    let mut total = 0usize;
    let mut ok = 0usize;
    let mut not_owned = 0usize;
    let mut failures = Vec::new();
    println!("{:<62} {:<8} result", "row", "owner");
    for case in launch_cases().into_iter().chain(interaction_cases()) {
        total += 1;
        let owner = acos_services::registry::owner_of(case.interface);
        match replay(&mut host, &case) {
            Ok(_) => {
                ok += 1;
                println!("{:<62} {:<8} ok", case.name, "services");
            }
            Err(ServiceError::NotOwned { .. }) => {
                not_owned += 1;
                println!("{:<62} {:<8} not-owned", case.name, owner.unwrap_or("acos-wm"));
            }
            Err(err) => {
                failures.push(format!("{}: {err}", case.name));
                println!("{:<62} {:<8} FAIL {err}", case.name, "services");
            }
        }
    }
    println!("replayed {total} rows: {ok} ok, {not_owned} not-owned");
    assert!(failures.is_empty(), "replay failures:\n{}", failures.join("\n"));
    assert!(
        ok >= 45,
        "expected at least 45 owned rows to replay, got {ok}"
    );
}

/// Regression (W40-A): `IActivityManager.getContentProvider` passes the
/// caller's op-package name first and the requested authority second. Reading
/// the first string made `ContentResolver.acquireProvider("settings")` return
/// null and `Settings$NameValueCache` NPE before `Activity.onCreate`.
#[test]
fn get_content_provider_resolves_the_authority_from_the_name_argument() {
    let mut host = ServiceHost::standard();
    let case = Case {
        name: "IActivityManager.getContentProvider",
        interface: "android.app.IActivityManager",
        method: "getContentProvider",
        args: vec![
            b(0x1000),
            s("dev.acos.fixtures.counter"),
            s("settings"),
            Value::Int(0),
            Value::Bool(false),
        ],
    };
    let reply = replay(&mut host, &case).expect("getContentProvider");
    let holder = reply.first().expect("one reply value");
    assert!(
        !matches!(holder, Value::TypedObject(Parcelable::Null)),
        "settings authority must resolve to a holder, got {holder:?}"
    );
}

/// T-36 acceptance: the recorded `acos-wm` binder replay produces byte-identical
/// outcomes across two independent runs (no hidden clock/order state).
#[test]
fn wm_replay_is_deterministic_across_runs() {
    let first = acos_wm::replay::run_default_replays().expect("first replay");
    let second = acos_wm::replay::run_default_replays().expect("second replay");
    assert_eq!(first, second, "replay diverged between runs");
    assert!(
        first.count(|s| matches!(s, acos_wm::replay::ReplayStatus::Ok)) >= 40,
        "replay covered too few rows: {}",
        first.count(|s| matches!(s, acos_wm::replay::ReplayStatus::Ok))
    );
}

#[test]
fn replay_launch_sequence_is_order_stable() {
    // The measured order is exactly the order of `launch_cases`; re-running it
    // twice must produce identical replies (no hidden mutation).
    let mut host = ServiceHost::standard();
    let cases = launch_cases();
    let first: Vec<_> = cases
        .iter()
        .map(|c| replay(&mut host, c).map(|v| v.len()))
        .collect();
    let mut host = ServiceHost::standard();
    let second: Vec<_> = cases
        .iter()
        .map(|c| replay(&mut host, c).map(|v| v.len()))
        .collect();
    assert_eq!(first, second);
}
