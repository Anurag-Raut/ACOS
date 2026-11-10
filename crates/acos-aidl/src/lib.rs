//! # acos-aidl
//!
//! Mechanically-derived AIDL transaction tables for the ACOS Rust service
//! layer. Every table entry is read out of the same Android 14 boot-classpath
//! DEX files the VM executes — nothing is hand-typed.
//!
//! The crate has two halves:
//!
//! * a scanner ([`decode`]) that turns a real `Foo$Stub` / `Foo$Stub$Proxy` pair
//!   into a typed [`InterfaceInfo`] (transaction code, method name, signature,
//!   oneway flag, and the ordered [`WireOp`] argument/reply layouts);
//! * a generated static table ([`generated::tables`]) for the default ACOS
//!   service-layer subset, plus the lookup helpers below.
//!
//! The generator is `cargo run -p acos-aidl --example gen -- --boot
//! .cache/boot/android-34` (add `--all` to dump every boot-classpath interface
//! to `results/aidl/all_interfaces.json`).

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod decode;
pub mod generate;
pub mod generated;
pub mod ir;

pub use decode::{decode_interface, try_decode_interface, DecodeError, Scanner};
pub use ir::{InterfaceInfo, MethodInfo, WireOp};

/// Every interface in the generated default subset, sorted by descriptor.
pub fn interfaces() -> &'static [InterfaceInfo] {
    generated::tables::INTERFACES.as_slice()
}

/// Look up an interface by dotted descriptor (`android.view.IWindowSession`).
pub fn interface_for_descriptor(descriptor: &str) -> Option<&'static InterfaceInfo> {
    interfaces().iter().find(|i| i.descriptor == descriptor)
}

/// Look up a transaction by `(descriptor, code)`.
pub fn lookup(descriptor: &str, code: u32) -> Option<&'static MethodInfo> {
    interface_for_descriptor(descriptor).and_then(|i| i.method_by_code(code))
}

/// Look up a transaction by `(descriptor, method name)`.
pub fn lookup_by_name(descriptor: &str, name: &str) -> Option<&'static MethodInfo> {
    interface_for_descriptor(descriptor).and_then(|i| i.method_by_name(name))
}

/// Iterate every `(interface, method)` pair in the generated table.
pub fn methods() -> impl Iterator<Item = (&'static InterfaceInfo, &'static MethodInfo)> {
    interfaces()
        .iter()
        .flat_map(|i| i.methods.iter().map(move |m| (i, m)))
}
