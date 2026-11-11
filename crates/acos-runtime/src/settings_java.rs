//! Settings-provider support for the runtime end of T-27.
//!
//! The Settings provider hands the framework a `MemoryIntArray`-backed
//! generation counter inside the reply `Bundle`. `Parcel.readParcelable`
//! instantiates it through `MemoryIntArray.CREATOR`, a static field populated
//! by the class's `<clinit>`.
//!
//! ACOS's `Class.forName`/reflection does not run `<clinit>` (the real ART
//! semantics trigger class initialization on a static read). Rather than let
//! the framework see a `null` `CREATOR`, the transport explicitly initialises
//! the class on the settings reply path — the same point Android would
//! initialise it, before the Java reader runs.
//!
//! The provider's on-wire generation value is still produced by
//! `acos-services::provider` (a real `memfd` + `ParcelFileDescriptor`); this
//! module only owns the Java-side class-initialisation seam.

use acos_vm::Vm;

/// Initialise the framework classes the settings reply instantiates through
/// their static `CREATOR` before the Java reader runs.
pub fn prepare_settings_reply(vm: &mut Vm) -> Result<(), String> {
    // `<clinit>` runs on the current thread's frame stack. Outside a VM run (a
    // test driving a binder transaction directly) there is no thread, and no
    // Java reader can consume the reply, so there is nothing to prepare.
    if vm.threads.is_empty() {
        return Ok(());
    }
    for desc in [
        "Landroid/util/MemoryIntArray;",
        "Landroid/os/ParcelFileDescriptor;",
    ] {
        let cid = vm
            .load_class(desc)
            .map_err(|e| format!("settings_java: load {desc}: {e}"))?;
        vm.initialize_class(cid)
            .map_err(|e| format!("settings_java: <clinit> {desc}: {e}"))?;
    }
    Ok(())
}
