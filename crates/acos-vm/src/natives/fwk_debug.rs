//! Debug-channel natives that are honest no-ops because ACOS has no debugger.
//!
//! `android.app.ActivityThread.attach` calls `DdmHandleAppName.setAppName`,
//! which forwards to `org.apache.harmony.dalvik.ddmc.DdmServer.nativeSendChunk`.
//! DDM is the Dalvik Debug Monitor wire protocol; on a device with **no
//! debugger attached** a DDM chunk is simply discarded. ACOS runs no JDWP/DDM
//! transport (ADR-0008 replaces zygote/art below the framework), so the only
//! correct behaviour is the same discard.
//!
//! This is a declared divergence, not an app-visible fake: no app API observes
//! it, and the framework treats the send as best-effort. Anything that would
//! change observable app behaviour stays a structured `UNSUPPORTED_JAVA_API`.

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::vm::Vm;

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let c = "Lorg/apache/harmony/dalvik/ddmc/DdmServer;";
    vm.register_native(c, "nativeSendChunk", "(I[BII)V", ddm_send_chunk);
    Ok(())
}

/// Discard a DDM chunk: no debugger is attached to the ACOS process.
fn ddm_send_chunk(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}
