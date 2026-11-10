//! # acos-vm
//!
//! A Rust implementation of an Android/Dalvik execution engine: DEX
//! interpreter, handle-based managed heap, precise mark-sweep GC, runtime
//! class model, core-library builtins and cooperative green threads.
//!
//! The crate is `#![forbid(unsafe_code)]` and never fakes an unimplemented
//! API: unknown classes, methods or opcodes stop execution with a structured
//! error naming the exact call site.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod bridge;
pub mod class;
pub mod codecache;
pub mod corelib;
mod dispatch;
pub mod error;
pub mod exec;
pub mod format;
pub mod frame;
pub mod heap;
pub mod interp;
pub mod javabridge;
pub mod limits;
pub mod natives;
pub mod snapshot;
pub mod vfs;
pub mod vm;

pub use error::{VmError, VmResult};
pub use heap::{Handle, Object};
pub use limits::HostGuard;
pub use vfs::{Vfs, VfsConfig};
pub use javabridge::{BridgeError, BridgeResult, JavaValue};
pub use vm::{
    descriptor_to_dotted, CoreLib, Metrics, NativeTrace, RunOutcome, Uncaught, Vm, VmOptions,
};
