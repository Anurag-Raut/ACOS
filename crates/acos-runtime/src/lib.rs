//! # acos-runtime
//!
//! Owns the pieces that must be wired together for the framework bytecode to
//! talk to the Rust service layer:
//!
//! * the [`Vm`] (real Android 14 core library + framework DEX),
//! * the in-process [`ServiceHost`] (mini PMS/ATMS/display/… state machines),
//! * the Binder transport ([`transport::ServiceTransport`]) installed into the
//!   VM's `BinderProxy.transactNative` seam.
//!
//! `acos-vm` deliberately does not depend on `acos-services`; this crate is the
//! one place that does, which keeps the VM crate free of service policy and the
//! services crate free of VM objects (see `research/service-layer/REPORT.md`
//! §3).
//!
//! ```
//! use acos_runtime::Runtime;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut rt = Runtime::standard()?;
//! // rt.vm_mut().add_dex(dex);
//! // let outcome = rt.run("Main", &[]);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod apk_loader;
pub mod atms_boot;
pub mod bind_app;
pub mod boot;
pub mod frame_handoff;
pub mod client_transaction;
pub mod display_java;
pub mod input_java;
pub mod lifecycle;
pub mod pms_java;
pub mod session_pool;
pub mod settings_java;
pub mod transport;
pub mod wms_java;

use std::cell::RefCell;
use std::rc::Rc;

use acos_parcel::registry::set_process_identity;
use acos_parcel::BinderId;
use acos_services::ServiceHost;
use acos_vm::natives::binder::{clear_transport, install_transport};
use acos_vm::{RunOutcome, Vm, VmError, VmOptions};

use crate::transport::ServiceTransport;

/// The uid/pid the framework sees for the app process.
pub const FIXTURE_UID: u32 = 10192;
/// Synthetic pid for the app process (ACOS is in-process).
pub const FIXTURE_PID: i32 = 1000;

pub use atms_boot::{AttachLog, AttachRecord};
pub use boot::{BootError, BootReport};
pub use lifecycle::{LifecycleError, LifecycleReport};

/// A booted ACOS runtime: VM + service host + Binder transport.
pub struct Runtime {
    vm: Vm,
    host: Rc<RefCell<ServiceHost>>,
}

impl core::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Runtime")
            .field("host", &self.host.borrow())
            .finish_non_exhaustive()
    }
}

impl Runtime {
    /// Build a runtime with an **empty** service host (tests register their own
    /// services; `IServiceManager` is still answered by the host itself).
    pub fn new(options: VmOptions) -> Result<Self, VmError> {
        Self::with_host(options, ServiceHost::new())
    }

    /// Build a runtime with the standard ACOS service set installed.
    pub fn standard() -> Result<Self, VmError> {
        Self::with_host(VmOptions::default(), ServiceHost::standard())
    }

    /// Build a runtime with explicit VM options and a standard service set.
    pub fn standard_with(options: VmOptions) -> Result<Self, VmError> {
        Self::with_host(options, ServiceHost::standard())
    }

    /// Build a runtime around an explicit `host`.
    pub fn with_host(options: VmOptions, host: ServiceHost) -> Result<Self, VmError> {
        let vm = Vm::new(options)?;
        let host = Rc::new(RefCell::new(host));
        let context = host
            .borrow()
            .binder_for_descriptor("android.os.IServiceManager")
            .unwrap_or(BinderId(0));
        set_process_identity(FIXTURE_UID, FIXTURE_PID);
        install_transport(Box::new(ServiceTransport::new(
            Rc::clone(&host),
            context,
            FIXTURE_UID,
            FIXTURE_PID,
        )));
        // The real `InputEventReceiver` ack path (`nativeFinishInputEvent`)
        // forwards to the service input dispatcher.
        crate::input_java::install();
        Ok(Self { vm, host })
    }

    /// The shared service host.
    #[must_use]
    pub fn host(&self) -> Rc<RefCell<ServiceHost>> {
        Rc::clone(&self.host)
    }

    /// The VM.
    #[must_use]
    pub fn vm(&self) -> &Vm {
        &self.vm
    }

    /// The VM, mutably.
    pub fn vm_mut(&mut self) -> &mut Vm {
        &mut self.vm
    }

    /// Run `main` with `args`.
    pub fn run(&mut self, main: &str, args: &[String]) -> RunOutcome {
        self.vm.run(main, args)
    }

}

impl Drop for Runtime {
    fn drop(&mut self) {
        clear_transport();
    }
}
