//! Further `handleBindApplication` natives found by driving the real M2 chain.
//!
//! These are genuine framework call sites on the path from
//! `ApplicationThread.bindApplication` to `Application.onCreate`. Each is
//! implemented with its real semantics for a sandbox that has no network
//! stack and no ART heap; nothing here fakes a lifecycle callback.

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::vm::Vm;

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    vm.register_native(
        "Landroid/net/NetworkUtils;",
        "getBoundNetworkHandleForProcess",
        "()J",
        get_bound_network_handle_for_process,
    );
    Ok(())
}

/// `NetworkUtils.getBoundNetworkHandleForProcess()`.
///
/// `handleBindApplication` asks for the process default proxy
/// (`ConnectivityManager.getDefaultProxy` -> `getBoundNetworkForProcess` ->
/// `getProcessDefaultNetwork` -> this native). On a device the native returns
/// the handle of the network the process was bound to, or `0` when nothing was
/// bound with `bindProcessToNetwork`. ACOS has no network stack and never binds
/// a process, so `0` is the real state, which makes `getProcessDefaultNetwork`
/// return `null` exactly like an unbound device process. It is not a faked
/// "online" answer.
fn get_bound_network_handle_for_process(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 0);
    Ok(())
}
