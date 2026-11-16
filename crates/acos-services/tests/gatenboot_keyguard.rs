//! GATENBOOT: `IWindowManager.isKeyguardLocked` / `isKeyguardSecure` answered
//! honestly on a device that has no keyguard.
//!
//! The failure this pins: with AOSP's per-process mainline bootstrap running
//! (`ActivityThread.initializeMainlineModules()`), Gaten's accessibility service
//! gets past `MediaSessionManager` and its own `onServiceConnected` asks
//! `KeyguardManager.isKeyguardLocked()`
//! (`KeyguardManager.java:579-585` -> `IWindowManager.isKeyguardLocked`,
//! image dex transaction 31). ACOS answered the structured
//! `UNSUPPORTED_ANDROID_API`, which `KeyguardManager` does not catch (it catches
//! only `RemoteException`), so the service process died while connecting and the
//! bubble window was never added.
//!
//! ACOS shows no lock screen, so "not locked / not secure" is the true answer,
//! not a default: AOSP's `KeyguardController.isKeyguardShowing()` /
//! `isKeyguardSecure()` are `false` while no keyguard exists, and the SIM-lock
//! half of `isKeyguardSecure` (`KeyguardManager.java:722-760`) cannot be true
//! when no SIM reports a PIN. Both are transactions on the real `window` service
//! node with the real interface token, i.e. the bytes the framework proxy sends.

use acos_parcel::Parcel;
use acos_services::exception::NO_EXCEPTION;
use acos_services::host::ServiceHost;

const IWINDOW_MANAGER: &str = "android.view.IWindowManager";

/// `IWindowManager.isKeyguardLocked()` and `isKeyguardSecure(int)` from the
/// image's `IWindowManager$Stub.getDefaultTransactionName` numbering (the
/// GATENBOOT run's structured error names code 31 for `isKeyguardLocked`).
const IS_KEYGUARD_LOCKED: u32 = 31;
const IS_KEYGUARD_SECURE: u32 = 32;

fn host() -> (ServiceHost, acos_parcel::BinderId) {
    let mut host = ServiceHost::new();
    acos_services::registry::install(&mut host);
    let binder = host
        .binder_for_descriptor(IWINDOW_MANAGER)
        .expect("the `window` service is installed by `registry::install`");
    (host, binder)
}

fn transact(
    host: &mut ServiceHost,
    binder: acos_parcel::BinderId,
    code: u32,
    data: &mut Parcel,
) -> Parcel {
    let mut reply = Parcel::new();
    host.transact(binder, code, data, &mut reply, 0)
        .expect("the window service answers every request with a reply");
    reply.set_data_position(0);
    reply
}

fn request(token: &str) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(token).expect("interface token");
    data
}

/// `KeyguardManager.isKeyguardLocked()`: `false`, with a real `writeNoException`
/// reply — not an unsupported error.
#[test]
fn is_keyguard_locked_answers_false() {
    let (mut host, binder) = host();
    let mut data = request(IWINDOW_MANAGER);
    data.set_data_position(0);
    let mut reply = transact(&mut host, binder, IS_KEYGUARD_LOCKED, &mut data);
    assert_eq!(
        reply.read_int32().expect("exception header"),
        NO_EXCEPTION,
        "isKeyguardLocked must not answer UNSUPPORTED_ANDROID_API"
    );
    assert_eq!(
        reply.read_int32().expect("boolean"),
        0,
        "no keyguard is showing"
    );
}

/// `KeyguardManager.isKeyguardSecure()` / `isDeviceSecure()`: `false` for the
/// caller's user — no keyguard, no SIM lock.
#[test]
fn is_keyguard_secure_answers_false() {
    let (mut host, binder) = host();
    let mut data = request(IWINDOW_MANAGER);
    data.write_int32(0); // userId
    data.set_data_position(0);
    let mut reply = transact(&mut host, binder, IS_KEYGUARD_SECURE, &mut data);
    assert_eq!(reply.read_int32().expect("exception header"), NO_EXCEPTION);
    assert_eq!(
        reply.read_int32().expect("boolean"),
        0,
        "no secure lock screen"
    );
}

/// An `IWindowManager` method ACOS does not answer still fails with the
/// structured error, and the error must name the interface that was called.
/// Before GATENBOOT the reply said `android.view.IWindowSession` for every
/// `IWindowManager` method (a hard-coded string), which hid which service was
/// missing the method.
#[test]
fn an_unsupported_window_manager_method_names_its_own_interface() {
    let (mut host, binder) = host();
    let mut data = request(IWINDOW_MANAGER);
    data.set_data_position(0);
    let mut reply = Parcel::new();
    // 900 is not a transaction in the image's IWindowManager table.
    let error = host
        .transact(binder, 900, &mut data, &mut reply, 0)
        .expect_err("an unknown transaction must be a structured error");
    let message = error.to_string();
    assert!(
        message.contains("android.view.IWindowManager"),
        "the error must name the interface that was called, got {message:?}"
    );
    assert!(
        !message.contains("android.view.IWindowSession"),
        "an IWindowManager method must not be reported as IWindowSession: {message:?}"
    );
    assert_eq!(
        error.code(),
        acos_services::codes::UNSUPPORTED_ANDROID_API,
        "no default success: {message}"
    );
}
