//! SPIKE-WV — the `webviewupdate` peer (ADR-0031 gate, `research/spike-wv/`).
//!
//! **Spike code.** Active only when `ACOS_SPIKE_WV=1`; see
//! `results/handoff/SPIKE-WV/REPORT.md` §3 for the wall this exists to pass and
//! for exactly what the real foundation must provide instead.
//!
//! Wall 3 of `WebViewFactory.getProvider()` is
//! `ServiceManager.getService("webviewupdate")` →
//! `IWebViewUpdateService.waitForAndGetProvider()`. The API-34 image's own
//! dex is the ground truth here (`.cache/spike-wv/dex/classes4.dump`,
//! `WebViewFactory.getWebViewContextAndSetProvider`):
//!
//! * a **null** service makes the framework invoke `waitForAndGetProvider` on
//!   null, and the `NullPointerException` is caught and rethrown as
//!   `MissingWebViewPackageException("Failed to load WebView provider: …")`;
//! * the reply is `Parcel.readTypedObject(WebViewProviderResponse.CREATOR)`:
//!   `writeTypedObject(packageInfo, flags)` then `writeInt(status)`;
//! * `status == 0` (`OK`) and `status == 3` (`PROVIDER_STARTED`) are accepted;
//!   anything else throws `MissingWebViewPackageException` naming the status.
//!
//! The transaction codes come from the same dump
//! (`IWebViewUpdateService$Stub$Proxy`, the `const` in front of each
//! `transact`): `1 notifyRelroCreationCompleted`, `2 waitForAndGetProvider`,
//! `3 changeProviderAndSetting`, `4 getValidWebViewPackages`,
//! `5 getAllWebViewPackages`, `6 getCurrentWebViewPackageName`,
//! `7 getCurrentWebViewPackage`, `8 isMultiProcessEnabled`,
//! `9 enableMultiProcess`. The provider-load path uses `2`; the probe app's own
//! `WebView.getCurrentWebViewPackage()` uses `7` **before** provider creation.
//! The rest are **not** answered — `raw_transact` returns `Ok(false)` and the
//! dispatcher reports the structured `UNSUPPORTED_ANDROID_API`, never a silent
//! success (`changeProviderAndSetting`/`enableMultiProcess` would otherwise be
//! a fake success: ACOS cannot change provider or run a multiprocess one).

use acos_parcel::Parcel;

use crate::error::Result;
use crate::host::{CallResult, ServiceObject};
use crate::value::Parcelable;

/// AIDL descriptor, byte-identical to the image's
/// `IWebViewUpdateService$Stub.DESCRIPTOR`.
pub const DESCRIPTOR: &str = "android.webkit.IWebViewUpdateService";

/// `IWebViewUpdateService.Stub.TRANSACTION_notifyRelroCreationCompleted`.
pub const TRANSACTION_NOTIFY_RELRO_CREATION_COMPLETED: u32 = 1;
/// `IWebViewUpdateService.Stub.TRANSACTION_waitForAndGetProvider`.
pub const TRANSACTION_WAIT_FOR_AND_GET_PROVIDER: u32 = 2;
/// `IWebViewUpdateService.Stub.TRANSACTION_getCurrentWebViewPackageName`.
pub const TRANSACTION_GET_CURRENT_WEBVIEW_PACKAGE_NAME: u32 = 6;
/// `IWebViewUpdateService.Stub.TRANSACTION_getCurrentWebViewPackage`.
pub const TRANSACTION_GET_CURRENT_WEBVIEW_PACKAGE: u32 = 7;
/// `IWebViewUpdateService.Stub.TRANSACTION_isMultiProcessEnabled`.
pub const TRANSACTION_IS_MULTIPROCESS_ENABLED: u32 = 8;

/// `WebViewProviderResponse.STATUS_OK`.
pub const STATUS_OK: i32 = 0;

/// The provider package the service advertises: the system package a phone
/// would have pre-installed in the image.
pub const PROVIDER_PACKAGE: &str = "com.android.webview";

/// `sourceDir`/`publicSourceDir` the PMS publishes for the provider package.
///
/// A **guest** path (the framework opens it through the sandbox VFS), exactly
/// like the launched app's `/data/app/<name>.apk`: the real foundation mounts
/// the provider APK there at session setup.
pub const PROVIDER_GUEST_SOURCE: &str = "/data/app/com.android.webview.apk";

/// `true` when the spike flag is set (`ACOS_SPIKE_WV=1`).
#[must_use]
pub fn enabled() -> bool {
    matches!(
        std::env::var("ACOS_SPIKE_WV").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// `IWebViewUpdateService` answered from one provider package record.
pub struct WebViewUpdateService {
    provider: Parcelable,
}

impl WebViewUpdateService {
    /// Build the service. It answers with the same `PackageInfo` the PMS
    /// serves for [`PROVIDER_PACKAGE`], so `WebViewFactory.verifyPackageInfo`
    /// compares a record against itself rather than against a second, hand
    /// written copy.
    #[must_use]
    pub fn new() -> Self {
        Self {
            provider: crate::pms::spike_webview_provider_package().to_parcelable_with_flags(0),
        }
    }
}

impl Default for WebViewUpdateService {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceObject for WebViewUpdateService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<CallResult> {
        Err(crate::error::ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: method.name.clone(),
        })
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        _data: &mut Parcel,
        reply: &mut Parcel,
    ) -> Result<bool> {
        if interface != DESCRIPTOR {
            return Ok(false);
        }
        match code {
            TRANSACTION_NOTIFY_RELRO_CREATION_COMPLETED => {
                // `void notifyRelroCreationCompleted()`: no payload. The device
                // records that the relro files are ready; ACOS has no relro
                // step, so completing it is the honest answer (the caller gets
                // no data back either way).
                crate::exception::write_no_exception(reply);
                Ok(true)
            }
            TRANSACTION_GET_CURRENT_WEBVIEW_PACKAGE => {
                // `PackageInfo getCurrentWebViewPackage()`: `readTypedObject
                // (PackageInfo.CREATOR)`, so presence int + payload. A device
                // answers with the current provider's record (null only when no
                // provider is installed); this service has exactly one.
                crate::exception::write_no_exception(reply);
                reply.write_int32(1);
                crate::parcelable::write_payload(reply, &self.provider)?;
                Ok(true)
            }
            TRANSACTION_IS_MULTIPROCESS_ENABLED => {
                // "Is the provider running multiple processes?" ACOS's provider
                // is in-process, so `false` is the true answer, not a stub.
                crate::exception::write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            TRANSACTION_GET_CURRENT_WEBVIEW_PACKAGE_NAME => {
                // `String` reply: `Parcel.readString()` after `readException()`.
                crate::exception::write_no_exception(reply);
                reply.write_string16(PROVIDER_PACKAGE)?;
                Ok(true)
            }
            TRANSACTION_WAIT_FOR_AND_GET_PROVIDER => {
                // `WebViewProviderResponse.writeToParcel`:
                //   writeTypedObject(packageInfo, flags) -> presence int + payload
                //   writeInt(status)
                crate::exception::write_no_exception(reply);
                reply.write_int32(1);
                crate::parcelable::write_payload(reply, &self.provider)?;
                reply.write_int32(STATUS_OK);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_parcel::Parcel;

    /// `1` = the object is present (the `writeTypedObject` presence word).
    const PRESENT: i32 = 1;
    /// `0` = no exception header (`Parcel.readException()` sees no throwable).
    const NO_EXCEPTION: i32 = 0;

    /// Rewind to the start of the reply and return its data length.
    fn rewind(p: &mut Parcel) -> usize {
        let end = p.data_position();
        p.set_data_position(0);
        end
    }

    #[test]
    fn wait_for_and_get_provider_writes_the_frameworks_expected_shape() {
        let mut service = WebViewUpdateService::new();
        let mut request = Parcel::new();
        let mut reply = Parcel::new();
        assert!(service
            .raw_transact(DESCRIPTOR, TRANSACTION_WAIT_FOR_AND_GET_PROVIDER, &mut request, &mut reply)
            .expect("raw_transact is infallible for this code"));

        let end = rewind(&mut reply);
        // `WebViewProviderResponse.writeToParcel`: presence, payload, status.
        assert_eq!(reply.read_int32().unwrap(), NO_EXCEPTION);
        assert_eq!(reply.read_int32().unwrap(), PRESENT);
        // The payload is a `PackageInfo`, whose first field is `packageName`
        // (`Parcel.writeString8` = byte length + UTF-8 + pad).
        assert_eq!(
            reply.read_string8().unwrap().as_deref(),
            Some(PROVIDER_PACKAGE)
        );
        // The reply ends with `status`; `STATUS_OK` is the only value that keeps
        // `WebViewFactory` on the success path.
        let mut tail = Vec::new();
        while reply.data_position() < end {
            tail.push(reply.read_int32().unwrap());
        }
        assert_eq!(tail.last().copied(), Some(STATUS_OK));
    }

    #[test]
    fn current_package_and_multi_process_answers_are_typed_and_honest() {
        let mut service = WebViewUpdateService::new();
        let mut request = Parcel::new();

        let mut reply = Parcel::new();
        assert!(service
            .raw_transact(DESCRIPTOR, TRANSACTION_GET_CURRENT_WEBVIEW_PACKAGE, &mut request, &mut reply)
            .unwrap());
        let _ = rewind(&mut reply);
        assert_eq!(reply.read_int32().unwrap(), NO_EXCEPTION);
        assert_eq!(reply.read_int32().unwrap(), PRESENT);
        assert_eq!(
            reply.read_string8().unwrap().as_deref(),
            Some(PROVIDER_PACKAGE)
        );

        // `isMultiProcessEnabled` is `false`: the provider runs in the app's own
        // process, so this is the true answer, not a stub.
        let mut reply = Parcel::new();
        assert!(service
            .raw_transact(DESCRIPTOR, TRANSACTION_IS_MULTIPROCESS_ENABLED, &mut request, &mut reply)
            .unwrap());
        let _ = rewind(&mut reply);
        assert_eq!(reply.read_int32().unwrap(), NO_EXCEPTION);
        assert_eq!(reply.read_int32().unwrap(), 0);
    }

    #[test]
    fn unimplemented_methods_are_structured_unsupported_never_a_silent_success() {
        let mut service = WebViewUpdateService::new();
        let mut request = Parcel::new();
        for code in [3u32, 4, 5, 9, 42] {
            let mut reply = Parcel::new();
            // `Ok(false)` = "this object does not serve that code"; the
            // dispatcher then writes the structured `UNSUPPORTED_ANDROID_API`.
            // Answering `Ok(true)` with an empty parcel would be a fake success.
            assert!(
                !service
                    .raw_transact(DESCRIPTOR, code, &mut request, &mut reply)
                    .unwrap(),
                "code {code} must not be answered"
            );
            assert_eq!(
                reply.data_position(),
                0,
                "code {code} must not write a reply"
            );
        }
        // A different interface is not ours either.
        let mut reply = Parcel::new();
        assert!(!service
            .raw_transact("android.content.pm.IPackageManager", 2, &mut request, &mut reply)
            .unwrap());
    }
}
