//! Minimal ATMS attach recorder for the M2 framework-boot stage.
//!
//! T-23 (the full lifecycle transaction stream) is a later task. Here we need
//! exactly one thing: the real `android.app.ActivityThread.attach(false, seq)`
//! path must reach the Rust `IActivityManager.attachApplication` handler and we
//! must be able to prove, from Rust, that it arrived with a **valid
//! `IApplicationThread` binder**.
//!
//! The existing [`acos_services::atms::AtmsService`] already implements the
//! handshake and keeps the process record internally. That state is not exposed
//! through [`ServiceHost`], so this module installs a thin **recording
//! decorator** over a fresh ATMS state: the decorator forwards every call
//! unchanged and additionally records `attachApplication`. It is deliberately
//! small and self-contained; the real ATMS state machines remain in
//! `acos-services`.
//!
//! Nothing is faked: if `attachApplication` arrives without a binder, the
//! record stores `None` and the caller must treat the handshake as failed.

use std::sync::{Arc, Mutex};

use acos_aidl::MethodInfo;
use acos_parcel::BinderId;
use acos_services::atms::{AtmsInterface, AtmsService, AtmsState};
use acos_services::host::{CallResult, ServiceHost, ServiceObject};
use acos_services::value::Value;
use acos_services::Result as ServiceResult;

/// One observed `IActivityManager.attachApplication` transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachRecord {
    /// The `IApplicationThread` binder the app passed (the `mAppThread`
    /// proxy). `None` means the argument decoded as `null`.
    pub app_thread: Option<BinderId>,
    /// The `startSeq` argument (`attachApplication(IApplicationThread, long)`).
    pub start_seq: i64,
}

/// Shared, ordered log of observed attach transactions.
pub type AttachLog = Arc<Mutex<Vec<AttachRecord>>>;

/// Replace the `activity` service with a recording ATMS wrapper and return the
/// shared log.
///
/// The wrapper delegates to a real [`AtmsService`], so the handshake state
/// machine (process record, lifecycle stream) still runs; it only adds the
/// observation the boot test needs.
pub fn install_recording_atms(host: &mut ServiceHost) -> AttachLog {
    // Reuse the host's ATMS state. It carries the process/attach records *and*
    // the `ProviderRegistry` that `IActivityManager.getContentProvider` answers
    // from (Q2b); a fresh `AtmsState` would silently empty the provider set, so
    // `ContentResolver.acquireProvider("settings")` would return null and the
    // first `Settings.Global.getInt` would NPE.
    let shared = host
        .ensure("android.app.IActivityManager")
        .ok()
        .and_then(|object| object.as_any_mut())
        .and_then(|any| any.downcast_mut::<AtmsService>())
        .map(|service| service.shared_state());
    let log: AttachLog = Arc::new(Mutex::new(Vec::new()));
    let log_for_factory = Arc::clone(&log);
    host.register("activity", "android.app.IActivityManager", move |wiring| {
        let state = shared
            .clone()
            .unwrap_or_else(|| Arc::new(Mutex::new(AtmsState::new())));
        let inner = AtmsService::new(AtmsInterface::ActivityManager, state, wiring);
        Box::new(RecordingAtms {
            inner,
            log: Arc::clone(&log_for_factory),
        })
    });
    log
}

struct RecordingAtms {
    inner: AtmsService,
    log: AttachLog,
}

impl core::fmt::Debug for RecordingAtms {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RecordingAtms").finish_non_exhaustive()
    }
}

impl ServiceObject for RecordingAtms {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        args: &[Value],
    ) -> ServiceResult<CallResult> {
        if method.name == "attachApplication" {
            let app_thread = args.first().and_then(Value::as_binder);
            let start_seq = args.get(1).and_then(Value::as_long).unwrap_or(0);
            if let Ok(mut log) = self.log.lock() {
                log.push(AttachRecord {
                    app_thread,
                    start_seq,
                });
            }
        }
        self.inner.call(interface, method, args)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        // Expose the wrapped real ATMS so a host query (T-43 attach re-seed)
        // can see the recorded process through the recording decorator.
        self.inner.as_any_mut()
    }
}

/// The first recorded attach, if any.
#[must_use]
pub fn first_attach(log: &AttachLog) -> Option<AttachRecord> {
    log.lock().ok().and_then(|l| l.first().cloned())
}

/// Assertion helper: a non-null binder is a valid `IApplicationThread` if the
/// recorded id is not the null binder.
#[must_use]
pub fn is_valid_app_thread(record: &AttachRecord) -> bool {
    record.app_thread.is_some_and(|b| !b.is_null())
}

