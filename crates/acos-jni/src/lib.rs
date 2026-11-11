//! # acos-jni
//!
//! A real JNI implementation for ACOS. The crate provides:
//!
//! * the complete 229-slot `JNINativeInterface` (plus the four reserved pointer
//!   slots) and the five `JNIInvokeInterface` entries, in exact NDK/`jni.h`
//!   order (see [`table`]);
//! * implementations backed by the ACOS VM ([`acos_vm::Vm`]) through
//!   `acos_vm::javabridge`: strings, classes, methods, fields, arrays,
//!   exceptions, local/global references and monitors;
//! * `Java_*` / `RegisterNatives` resolution and Java → native dispatch with a
//!   real `JNIEnv*`, wired to `acos-native-loader` and `JNI_OnLoad`;
//! * `System.loadLibrary` / `Runtime.nativeLoad` through the VM's
//!   `natives::jni_glue` seam.
//!
//! ## Error model
//!
//! JNI is a C ABI with no error channel. ACOS never guesses:
//!
//! * a pending exception plus a non-exception-safe call records
//!   [`JniError::PendingException`] and aborts the native call;
//! * null/bad references, wrong types and missing IDs record a structured
//!   [`JniError`] and abort the call (or throw the Android exception for the
//!   lookup APIs, e.g. `NoSuchMethodError`);
//! * an unimplemented slot records [`JniError::Unsupported`] and aborts the
//!   native call cleanly — the host process never aborts and no default value
//!   is silently returned to Java.
//!
//! ## Usage
//!
//! ```
//! use acos_jni::{JniConfig, JniRuntime};
//! use acos_vm::{Vm, VmOptions};
//!
//! let mut vm = Vm::new(VmOptions::default()).unwrap();
//! let jni = JniRuntime::new(JniConfig::default().with_search_path("fixtures/build/apps/hello-jni-jni/x86_64"));
//! jni.attach(&mut vm).unwrap();
//! // vm.add_dex(...); then `jni.scan_natives(&mut vm)` for DEX added after attach.
//! # let _ = (&jni, &vm);
//! ```

#![allow(clippy::missing_safety_doc)]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_debug_implementations)]

pub mod broker;
pub mod dispatch;
pub mod error;
pub mod flutter;
pub mod flutter_session;
pub mod jni;
pub mod load;
pub mod react_native;
pub mod gles_host;
pub mod hostmem;
pub mod ndk_host;
pub mod refs;
pub mod state;
pub mod table;
pub mod traps;

pub use broker::{attached_workers, pending_requests, take_worker_error};
pub use error::JniError;
pub use jni::{acos_jni_method_codes, acos_jni_va_invoke, implemented_slot_count, JniCallResult};
pub use react_native::{ReactNativeApk, ReactNativeError, RnNode, RnTree};
pub use state::{JniConfig, JniRuntime, JNI_ERR, JNI_OK, JNI_VERSION_1_4, JNI_VERSION_1_6};
pub use flutter_session::{
    FlutterFrame, FlutterSession, FlutterSessionError, FlutterSessionOptions,
};

/// Stable identity of the JNI table (test/debug aid).
pub const JNI_SLOT_COUNT: usize = table::JNI_SLOT_COUNT;

/// Service queued brokered JNI requests against the thread-local owner VM.
///
/// The VM's safe points call this automatically; native code may call it while
/// the owner is blocked in a native call that is waiting on a worker, so the
/// synchronous owner→native→worker→Java→owner pattern cannot deadlock.
#[no_mangle]
pub extern "C" fn acos_jni_broker_pump() -> usize {
    broker::service_current()
}

/// Number of requests queued across all owners (diagnostic).
#[no_mangle]
pub extern "C" fn acos_jni_broker_pending() -> usize {
    broker::pending_requests()
}
