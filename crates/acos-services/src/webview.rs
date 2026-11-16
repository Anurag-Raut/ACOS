//! The `webviewupdate` peer: `IWebViewUpdateService` (ADR-0031).
//!
//! The provider package is an ordinary installed system package (ADR-0031: the
//! host's Chromium behind an ACOS WebView provider, loaded by the framework's
//! `WebViewFactory`). This service answers the framework's provider query from
//! the same package table `PackageManager` uses (`crate::pms`).
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
/// `WebViewProviderResponse.STATUS_MISSING_ZYGOTE` — no provider at all.
pub const STATUS_MISSING_ZYGOTE: i32 = 1;

/// `PackageManager.FEATURE_WEBVIEW`: true exactly when the provider package is
/// installed ([`crate::pms::PmsState::has_system_feature`]).
pub const WEBVIEW_FEATURE: &str = "android.software.webview";

/// The provider package the service advertises: the system package a phone
/// would have pre-installed in the image.
pub const PROVIDER_PACKAGE: &str = "com.android.webview";

/// `sourceDir`/`publicSourceDir` the PMS publishes for the provider package.
///
/// A **guest** path (the framework opens it through the sandbox VFS), exactly
/// like the launched app's `/data/app/<name>.apk`: the real foundation mounts
/// the provider APK there at session setup.
pub const PROVIDER_GUEST_SOURCE: &str = "/data/app/com.android.webview.apk";

/// `IWebViewUpdateService` answered from the session's package table.
///
/// The answer is the **installed** provider record, not a second hand-written
/// copy: `WebViewFactory.verifyPackageInfo` compares the `waitForAndGetProvider`
/// record against `PackageManager.getPackageInfo(providerPackage, 0x100024c0)`
/// on signatures and `versionCode`, so two records for one package would be a
/// latent mismatch. The state handle is the same `Arc<Mutex<PmsState>>` the
/// `package` service mutates at launch.
pub struct WebViewUpdateService {
    pms: std::sync::Arc<std::sync::Mutex<crate::pms::PmsState>>,
}

impl WebViewUpdateService {
    /// Build the service over the session's package state.
    #[must_use]
    pub fn new(pms: std::sync::Arc<std::sync::Mutex<crate::pms::PmsState>>) -> Self {
        Self { pms }
    }

    /// The provider record, when the session image has the package installed.
    fn provider(&self) -> Option<Parcelable> {
        self.pms
            .lock()
            .ok()?
            .lookup(Some(PROVIDER_PACKAGE))
            .map(|record| record.package.to_parcelable_with_flags(0))
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
        let provider = self.provider();
        let handled = match code {
            TRANSACTION_NOTIFY_RELRO_CREATION_COMPLETED => {
                // `void notifyRelroCreationCompleted()`: no payload. The device
                // records that the relro files are ready; ACOS has no relro
                // step, so completing it is the honest answer (the caller gets
                // no data back either way).
                crate::exception::write_no_exception(reply);
                true
            }
            TRANSACTION_GET_CURRENT_WEBVIEW_PACKAGE => {
                // `PackageInfo getCurrentWebViewPackage()`: `readTypedObject
                // (PackageInfo.CREATOR)`, so presence int + payload. Null is
                // the true answer when no provider package is installed — the
                // same answer a phone without a WebView gives.
                crate::exception::write_no_exception(reply);
                match &provider {
                    Some(info) => {
                        reply.write_int32(1);
                        crate::parcelable::write_payload(reply, info)?;
                    }
                    None => reply.write_int32(0),
                }
                true
            }
            TRANSACTION_IS_MULTIPROCESS_ENABLED => {
                // "Is the provider running multiple processes?" ACOS's provider
                // is in-process, so `false` is the true answer, not a stub.
                crate::exception::write_no_exception(reply);
                reply.write_int32(0);
                true
            }
            TRANSACTION_GET_CURRENT_WEBVIEW_PACKAGE_NAME => {
                // `String` reply: `Parcel.readString()` after `readException()`.
                crate::exception::write_no_exception(reply);
                match provider.is_some() {
                    true => reply.write_string16(PROVIDER_PACKAGE)?,
                    false => reply.write_string16_opt(None)?,
                }
                true
            }
            TRANSACTION_WAIT_FOR_AND_GET_PROVIDER => {
                // The method returns a **parcelable**, so the wire is
                // `writeTypedObject(response, flags)` — a presence marker for
                // the response — and `WebViewProviderResponse.writeToParcel`
                // then writes `writeTypedObject(packageInfo, flags)` (a second
                // presence marker), the payload, and `writeInt(status)`.
                //
                // WV2 wall 4: writing only the inner marker shifted the reply
                // by 4 bytes; the image's reader took `packageName`'s length
                // for the `packageInfo` presence, then read `splitNames`'s
                // count out of the string bytes and threw
                // `OutOfMemoryError: Failed to allocate a 7676737936 byte
                // allocation at Parcel.createString8Array`
                // (`PackageInfo.<init>(PackageInfo.java:570)`, the `splitNames`
                // read). The proxy's reader is in the image's
                // `classes4.dex:Landroid/webkit/IWebViewUpdateService$Stub$Proxy;->waitForAndGetProvider`
                // (`readException`, `readTypedObject(WebViewProviderResponse.CREATOR)`),
                // and the response's own reader is
                // `WebViewProviderResponse.<init>(Parcel)`
                // (`readTypedObject(PackageInfo.CREATOR)` then `readInt`).
                //
                // `STATUS_MISSING_ZYGOTE` is what a phone with no provider
                // reports; the framework turns any non-OK status into
                // `MissingWebViewPackageException`, which is the honest answer.
                crate::exception::write_no_exception(reply);
                reply.write_int32(1); // readTypedObject(WebViewProviderResponse.CREATOR)
                match &provider {
                    Some(info) => {
                        reply.write_int32(1); // writeTypedObject(packageInfo)
                        crate::parcelable::write_payload(reply, info)?;
                        reply.write_int32(STATUS_OK);
                    }
                    None => {
                        reply.write_int32(0); // packageInfo = null: honest "no provider"
                        reply.write_int32(STATUS_MISSING_ZYGOTE);
                    }
                }
                true
            }
            _ => false,
        };
        if handled {
            // WV2: `ACOS_PARCEL_DUMP` records the exact reply bytes the guest
            // reads. This reply is written by the Rust codec (no Java
            // materialiser), which is why it was the first place a codec/spec
            // mismatch surfaced as a framework-side `PackageInfo` desync.
            crate::parcel_dump::dump(&format!("webviewupdate.{code}"), reply);
        }
        Ok(handled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_parcel::Parcel;

    /// The session package state with the provider package installed: the same
    /// table `PackageManager` serves, so the peer's reply is the record the
    /// framework compares against itself.
    fn test_state_with_provider() -> std::sync::Arc<std::sync::Mutex<crate::pms::PmsState>> {
        let mut state = crate::pms::PmsState::hello_views();
        let mut record = state.packages[state.primary].clone();
        record.package.package_name = PROVIDER_PACKAGE.into();
        record.app.package_name = PROVIDER_PACKAGE.into();
        record.package.application = record.app.clone();
        state.packages.push(record);
        std::sync::Arc::new(std::sync::Mutex::new(state))
    }

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
        let mut service = WebViewUpdateService::new(test_state_with_provider());
        let mut request = Parcel::new();
        let mut reply = Parcel::new();
        assert!(service
            .raw_transact(DESCRIPTOR, TRANSACTION_WAIT_FOR_AND_GET_PROVIDER, &mut request, &mut reply)
            .expect("raw_transact is infallible for this code"));

        let end = rewind(&mut reply);
        // The framework's reader order, from the image's own dex:
        //   readException
        //   readTypedObject(WebViewProviderResponse.CREATOR) -> presence
        //   WebViewProviderResponse.<init>(Parcel):
        //       readTypedObject(PackageInfo.CREATOR) -> presence
        //       PackageInfo payload
        //       readInt(status)
        // Two presence words, not one: the method returns a parcelable *and*
        // that parcelable writes a nested one (WV2 wall 4).
        assert_eq!(reply.read_int32().unwrap(), NO_EXCEPTION);
        assert_eq!(reply.read_int32().unwrap(), PRESENT, "WebViewProviderResponse presence");
        assert_eq!(reply.read_int32().unwrap(), PRESENT, "packageInfo presence");
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
        let mut service = WebViewUpdateService::new(test_state_with_provider());
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
        let mut service = WebViewUpdateService::new(test_state_with_provider());
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

    /// No provider package installed = the answer a phone without a WebView
    /// gives: a null record and a non-OK status, never a fabricated package.
    #[test]
    fn without_the_provider_package_the_answer_is_no_provider() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(crate::pms::PmsState::hello_views()));
        let mut service = WebViewUpdateService::new(state);
        let mut request = Parcel::new();

        let mut reply = Parcel::new();
        assert!(service
            .raw_transact(DESCRIPTOR, TRANSACTION_WAIT_FOR_AND_GET_PROVIDER, &mut request, &mut reply)
            .unwrap());
        let end = rewind(&mut reply);
        assert_eq!(reply.read_int32().unwrap(), NO_EXCEPTION);
        // The response itself is present (the framework would NPE on a null
        // one); its `packageInfo` is `writeTypedObject(null)` = presence 0 and
        // no payload, then the honest non-OK status.
        assert_eq!(reply.read_int32().unwrap(), PRESENT, "WebViewProviderResponse presence");
        assert_eq!(reply.read_int32().unwrap(), 0, "packageInfo presence");
        assert_eq!(reply.read_int32().unwrap(), STATUS_MISSING_ZYGOTE);
        assert_eq!(reply.data_position(), end, "a null record writes no payload");

        let mut reply = Parcel::new();
        assert!(service
            .raw_transact(DESCRIPTOR, TRANSACTION_GET_CURRENT_WEBVIEW_PACKAGE, &mut request, &mut reply)
            .unwrap());
        let _ = rewind(&mut reply);
        assert_eq!(reply.read_int32().unwrap(), NO_EXCEPTION);
        assert_eq!(reply.read_int32().unwrap(), 0);
    }
}
