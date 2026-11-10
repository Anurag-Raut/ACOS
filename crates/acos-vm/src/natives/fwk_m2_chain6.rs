//! `com.android.internal.util.VirtualRefBasePtr` — the native refcount wrapper
//! the framework uses for `VectorDrawable` trees, `TextureLayer`,
//! `CanvasProperty` and `RenderNodeAnimator`.
//!
//! `VectorDrawable.<init>` wraps the `nCreateTree` pointer in a
//! `VirtualRefBasePtr`, whose constructor calls `nIncStrong(ptr)` and whose
//! finalizer calls `nDecStrong(ptr)`. ACOS keeps a real count per pointer:
//! `nIncStrong` increments, `nDecStrong` decrements, and when the count reaches
//! zero the `VectorDrawable` tree handle is released. Pointers owned by other
//! `VirtualRefBasePtr` users are counted too but only the drawable tree is
//! released here; their owners free them through their own natives.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::vm::Vm;

static REFCOUNTS: OnceLock<Mutex<HashMap<i64, u64>>> = OnceLock::new();

fn refcounts() -> MutexGuard<'static, HashMap<i64, u64>> {
    let m = REFCOUNTS.get_or_init(|| Mutex::new(HashMap::new()));
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    const C: &str = "Lcom/android/internal/util/VirtualRefBasePtr;";
    vm.register_native(C, "nIncStrong", "(J)V", n_inc_strong);
    vm.register_native(C, "nDecStrong", "(J)V", n_dec_strong);
    Ok(())
}

fn n_inc_strong(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let p = vm.arg_i64(ctx, 0);
    if p != 0 {
        let mut table = refcounts();
        *table.entry(p).or_insert(0) += 1;
    }
    Ok(())
}

fn n_dec_strong(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let p = vm.arg_i64(ctx, 0);
    if p != 0 {
        let mut release = false;
        {
            let mut table = refcounts();
            if let Some(count) = table.get_mut(&p) {
                *count = count.saturating_sub(1);
                release = *count == 0;
            }
            if release {
                table.remove(&p);
            }
        }
        if release {
            crate::natives::gfx_vector::release_tree(p);
        }
    }
    Ok(())
}
