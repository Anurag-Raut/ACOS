//! Process-global cache of **decoded method bodies**.
//!
//! A [`MethodCode`](crate::class::MethodCode) is a pure function of the DEX
//! bytes and the `code_off` of one `code_item`: decoding it never consults VM
//! state. It is therefore immutable research data that every session in one
//! process can share. The boot framework executes the same several thousand
//! method bodies in every session, so decoding them once (not once per session)
//! removes both the CPU cost and the per-session `insns`/`raw`/`pc_to_idx`
//! allocations.
//!
//! The key is `(DexFile::id, code_off)`. `DexFile::id` is process-unique and is
//! shared when the parsed file is shared (see `acos_dex::cache`), so two VMs
//! that share a boot DEX hit the same entries. Two separately-parsed copies of
//! the same file get different ids and do not share; that is safe (the decode is
//! still correct) and only loses the optimisation.
//!
//! Decoding happens outside the lock; a lost race re-uses the winner's body.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use acos_dex::DexFile;

use crate::class::MethodCode;
use crate::error::VmResult;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct CodeKey {
    dex: u64,
    code_off: u32,
}

static CACHE: OnceLock<Mutex<HashMap<CodeKey, Arc<MethodCode>>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<CodeKey, Arc<MethodCode>>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Return the shared body for `code_off` in `file`, decoding it with `decode`
/// only on the first call.
pub fn get_or_decode<F>(file: &DexFile, code_off: u32, decode: F) -> VmResult<Arc<MethodCode>>
where
    F: FnOnce() -> VmResult<MethodCode>,
{
    let key = CodeKey {
        dex: file.id(),
        code_off,
    };
    if let Some(code) = cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned()
    {
        return Ok(code);
    }

    let decoded = Arc::new(decode()?);
    let mut map = cache().lock().unwrap_or_else(|e| e.into_inner());
    Ok(map.entry(key).or_insert(decoded).clone())
}

/// Number of distinct decoded method bodies currently resident in the process.
pub fn len() -> usize {
    cache().lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// Drop every entry. Existing `Arc` owners keep their body alive.
pub fn clear() {
    cache().lock().unwrap_or_else(|e| e.into_inner()).clear();
}
