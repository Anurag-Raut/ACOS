//! Off-by-default launch profiler (`ACOS_PROFILE=1`).
//!
//! Every instrumentation site is gated on [`enabled`], which after the first
//! call is a single relaxed atomic load, so a disabled profiler costs one
//! predictable branch per class load / native call / GC and nothing in the
//! interpreter's hot loop.
//!
//! When enabled the module accumulates wall time and counts for the launch
//! pipeline: per-stage wall time, executed bytecode, class link/parse counts
//! and time, native call counts/time by `(class, name)`, GC count/time,
//! `<clinit>` count/time by class, and thrown-exception count. [`report`]
//! renders a single JSON object; [`emit_if_enabled`] writes it to
//! `$ACOS_PROFILE_OUT` (a path) or stderr.
//!
//! The profiler never changes observable behaviour: it only reads counters
//! that the VM would otherwise discard. Timings use `Instant` at coarse
//! boundaries (class loads, native calls, GC, clinit, phases), not per
//! instruction.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::vm::{Vm, descriptor_to_dotted};

static ENABLED: AtomicBool = AtomicBool::new(false);
pub(crate) static INIT: std::sync::Once = std::sync::Once::new();

/// True when the profiler has been turned on by `ACOS_PROFILE`.
///
/// The common (disabled) path is one relaxed atomic load. The first call
/// initialises the flag from the environment under a `Once`.
#[inline(always)]
pub fn enabled() -> bool {
    if ENABLED.load(Ordering::Relaxed) {
        return true;
    }
    init_and_check()
}

#[cold]
fn init_and_check() -> bool {
    // Set after init so the hot path above short-circuits.
    INIT.call_once(|| {
        let on = std::env::var_os("ACOS_PROFILE")
            .map(|v| !(v == "0" || v.is_empty()))
            .unwrap_or(false);
        ENABLED.store(on, Ordering::Relaxed);
    });
    ENABLED.load(Ordering::Relaxed)
}

/// Force initialisation (used by tests / callers that want a definite answer).
pub fn init() {
    let _ = init_and_check();
}

macro_rules! counter {
    ($name:ident) => {
        static $name: AtomicU64 = AtomicU64::new(0);
    };
}

counter!(INSTRUCTIONS);
counter!(CLASS_LINKS);
counter!(CLASS_PARSES);
counter!(CLASS_LINK_NS);
counter!(CLASS_PARSE_NS);
counter!(NATIVE_CALLS);
counter!(NATIVE_NS);
counter!(GC_COUNT);
counter!(GC_NS);
counter!(CLINIT_COUNT);
counter!(CLINIT_NS);
counter!(EXCEPTIONS);

#[inline]
fn bump(c: &AtomicU64, n: u64) {
    c.fetch_add(n, Ordering::Relaxed);
}

/// Start a timed region. Returns `None` when the profiler is off so the
/// caller's `if let`/`match` is the only overhead.
#[inline(always)]
pub fn start() -> Option<Instant> {
    if enabled() {
        Some(Instant::now())
    } else {
        None
    }
}

/// Accumulate `n` executed bytecode instructions.
#[inline]
pub fn add_instructions(n: u64) {
    if enabled() {
        bump(&INSTRUCTIONS, n);
    }
}

/// Record a completed class link (`link_class`) with its descriptor.
#[inline]
pub fn record_class_link(vm: &Vm, id: crate::class::ClassId, start: Option<Instant>) {
    let Some(start) = start else { return };
    let ns = start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
    bump(&CLASS_LINKS, 1);
    bump(&CLASS_LINK_NS, ns);
    let map = class_links();
        if let Ok(mut m) = map.lock() {
            let e = m
                .entry(descriptor_to_dotted(vm.class_desc(id)))
                .or_insert((0, 0));
            e.0 += 1;
            e.1 += ns;
        }
}

/// Record a completed class parse/load (`load_dex_class_body`) with its descriptor.
#[inline]
pub fn record_class_parse(desc: &str, start: Option<Instant>) {
    let Some(start) = start else { return };
    let ns = start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
    bump(&CLASS_PARSES, 1);
    bump(&CLASS_PARSE_NS, ns);
    let map = class_parses();
        if let Ok(mut m) = map.lock() {
            let e = m.entry(descriptor_to_dotted(desc)).or_insert((0, 0));
            e.0 += 1;
            e.1 += ns;
        }
}

/// Begin a native call. The `(class, name)` key is built lazily at the first
/// call of each method and cached, so a hot native (e.g. `String.charAt`,
/// hundreds of thousands of calls) costs one mutex lookup, not a `format!`.
#[inline]
pub fn native_begin(
    vm: &Vm,
    method: crate::class::MethodId,
) -> Option<(crate::class::MethodId, Instant, String)> {
    if !enabled() {
        return None;
    }
    let key = {
        let map = native_keys();
        let mut m = map.lock().ok()?;
        if let Some(k) = m.get(&method) {
            k.clone()
        } else {
            let mm = &vm.methods[method as usize];
            let k = format!(
                "{}.{}",
                descriptor_to_dotted(vm.class_desc(mm.class)),
                mm.name
            );
            m.insert(method, k.clone());
            k
        }
    };
    Some((method, Instant::now(), key))
}

/// Finish a native call started with [`native_begin`].
#[inline]
pub fn native_end(v: Option<(crate::class::MethodId, Instant, String)>) {
    let Some((_method, start, key)) = v else { return };
    let ns = start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
    bump(&NATIVE_CALLS, 1);
    bump(&NATIVE_NS, ns);
    let map = natives();
    if let Ok(mut m) = map.lock() {
        let e = m.entry(key).or_insert((0, 0));
        e.0 += 1;
        e.1 += ns;
    }
}

/// Record one GC cycle (the caller passes the `gc()` start instant).
#[inline]
pub fn record_gc(start: Instant) {
    if !enabled() {
        return;
    }
    bump(&GC_COUNT, 1);
    bump(
        &GC_NS,
        start.elapsed().as_nanos().min(u64::MAX as u128) as u64,
    );
}

/// Begin a `<clinit>` body for class `desc`.
#[inline]
pub fn clinit_begin(desc: &str) -> Option<(String, Instant)> {
    if !enabled() {
        return None;
    }
    Some((descriptor_to_dotted(desc), Instant::now()))
}

/// Finish a `<clinit>` started with [`clinit_begin`].
#[inline]
pub fn clinit_end(v: Option<(String, Instant)>) {
    let Some((key, start)) = v else { return };
    let ns = start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
    bump(&CLINIT_COUNT, 1);
    bump(&CLINIT_NS, ns);
    let map = clinits();
        if let Ok(mut m) = map.lock() {
            let e = m.entry(key).or_insert((0, 0));
            e.0 += 1;
            e.1 += ns;
        }
}

/// Record one thrown Java exception.
#[inline]
pub fn record_exception() {
    if enabled() {
        bump(&EXCEPTIONS, 1);
    }
}

/// Add wall time to a named stage. Repeat calls accumulate.
pub fn add_stage(name: &str, ns: u64) {
    if !enabled() {
        return;
    }
    let map = stages();
        if let Ok(mut m) = map.lock() {
            let e = m.entry(name.to_string()).or_insert((0, 0));
            e.0 += 1;
            e.1 += ns;
        }
}

/// Time `f` as a named stage (only when the profiler is on).
pub fn stage<R>(name: &str, f: impl FnOnce() -> R) -> R {
    if !enabled() {
        return f();
    }
    let s = Instant::now();
    let r = f();
    add_stage(name, s.elapsed().as_nanos().min(u64::MAX as u128) as u64);
    r
}

/// Attach an integer note (e.g. heap bytes) to the report.
pub fn set_note(key: &str, value: u64) {
    if !enabled() {
        return;
    }
    let map = notes();
        if let Ok(mut m) = map.lock() {
            m.insert(key.to_string(), value);
        }
}

fn stages() -> &'static Mutex<HashMap<String, (u64, u64)>> {
    static M: OnceLock<Mutex<HashMap<String, (u64, u64)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}
fn natives() -> &'static Mutex<HashMap<String, (u64, u64)>> {
    static M: OnceLock<Mutex<HashMap<String, (u64, u64)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}
fn native_keys() -> &'static Mutex<HashMap<crate::class::MethodId, String>> {
    static M: OnceLock<Mutex<HashMap<crate::class::MethodId, String>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}
fn class_links() -> &'static Mutex<HashMap<String, (u64, u64)>> {
    static M: OnceLock<Mutex<HashMap<String, (u64, u64)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}
fn class_parses() -> &'static Mutex<HashMap<String, (u64, u64)>> {
    static M: OnceLock<Mutex<HashMap<String, (u64, u64)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}
fn clinits() -> &'static Mutex<HashMap<String, (u64, u64)>> {
    static M: OnceLock<Mutex<HashMap<String, (u64, u64)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}
fn notes() -> &'static Mutex<HashMap<String, u64>> {
    static M: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Current process resident set size in bytes, from `/proc/self/statm`.
///
/// Returns `0` on platforms without procfs; the launch host is Linux, so this
/// is the RSS the timing harness reports.
pub fn rss_bytes() -> u64 {
    let Ok(s) = std::fs::read_to_string("/proc/self/statm") else {
        return 0;
    };
    let pages: u64 = s
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    pages.saturating_mul(4096)
}

/// Build the profile JSON object.
pub fn report() -> serde_json::Value {
    use serde_json::json;

    let sorted = |m: &Mutex<HashMap<String, (u64, u64)>>| -> Vec<serde_json::Value> {
        let mut v: Vec<(String, (u64, u64))> = match m.lock() {
            Ok(g) => g.iter().map(|(k, x)| (k.clone(), *x)).collect(),
            Err(_) => Vec::new(),
        };
        v.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
        v.into_iter()
            .map(|(k, (c, ns))| json!({"name": k, "count": c, "ns": ns}))
            .collect()
    };

    let stage_list = |m: &Mutex<HashMap<String, (u64, u64)>>| -> Vec<serde_json::Value> {
        let mut v: Vec<(String, (u64, u64))> = match m.lock() {
            Ok(g) => g.iter().map(|(k, x)| (k.clone(), *x)).collect(),
            Err(_) => Vec::new(),
        };
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v.into_iter()
            .map(|(k, (c, ns))| json!({"name": k, "count": c, "ns": ns}))
            .collect()
    };

    let notes_v = notes().lock().ok().map(|g| g.clone()).unwrap_or_default();

    json!({
        "enabled": true,
        "rss_bytes": rss_bytes(),
        "stages": stage_list(stages()),
        "counters": {
            "instructions": INSTRUCTIONS.load(Ordering::Relaxed),
            "class_links": CLASS_LINKS.load(Ordering::Relaxed),
            "class_parses": CLASS_PARSES.load(Ordering::Relaxed),
            "class_link_ns": CLASS_LINK_NS.load(Ordering::Relaxed),
            "class_parse_ns": CLASS_PARSE_NS.load(Ordering::Relaxed),
            "native_calls": NATIVE_CALLS.load(Ordering::Relaxed),
            "native_ns": NATIVE_NS.load(Ordering::Relaxed),
            "gc_count": GC_COUNT.load(Ordering::Relaxed),
            "gc_ns": GC_NS.load(Ordering::Relaxed),
            "clinit_count": CLINIT_COUNT.load(Ordering::Relaxed),
            "clinit_ns": CLINIT_NS.load(Ordering::Relaxed),
            "exceptions": EXCEPTIONS.load(Ordering::Relaxed),
        },
        "natives": sorted(natives()),
        "class_parses_by_class": sorted(class_parses()),
        "class_links_by_class": sorted(class_links()),
        "clinit_by_class": sorted(clinits()),
        "notes": notes_v,
    })
}

/// Write the profile JSON to `$ACOS_PROFILE_OUT`, or stderr when unset.
pub fn emit_if_enabled() {
    if !enabled() {
        return;
    }
    let report = report();
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    match std::env::var_os("ACOS_PROFILE_OUT") {
        Some(path) if !path.is_empty() => {
            if let Err(e) = std::fs::write(&path, &text) {
                eprintln!("ACOS_PROFILE: writing {}: {e}", path.to_string_lossy());
            }
        }
        _ => eprintln!("{text}"),
    }
}

/// Record the heap/class footprint an `emit` should carry, from one `Vm`.
pub fn note_vm_footprint(vm: &Vm) {
    if !enabled() {
        return;
    }
    let heap_live: u64 = vm.heap.live_bytes as u64;
    let heap_peak: u64 = vm.heap.peak_bytes as u64;
    set_note("vm_heap_live_bytes", heap_live);
    set_note("vm_heap_peak_bytes", heap_peak);
    set_note("vm_classes", vm.classes.len() as u64);
    set_note("vm_methods", vm.methods.len() as u64);
    set_note("vm_dex_files", vm.dexs.len() as u64);
    set_note("vm_threads", vm.threads.len() as u64);

    // Memory accounting by category. The guest heap is small (tens of MiB);
    // the RSS is dominated by raw DEX bytes and the decoded MethodCode copies,
    // so name each explicitly. `Arc<MethodCode>` is shared by every VM in the
    // process (process-global code cache); dedupe by pointer so this is the
    // retained size, not a per-VM overcount.
    let insn_size = std::mem::size_of::<crate::class::Insn>() as u64;
    let mut dex_bytes: u64 = 0;
    for d in &vm.dexs {
        dex_bytes += d.file.bytes().len() as u64;
    }
    let mut code_bytes: u64 = 0;
    let mut method_meta_bytes: u64 = 0;
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for m in &vm.methods {
        method_meta_bytes += (m.name.len() + m.sig.len() + m.ins_ref.len()) as u64;
        if let crate::class::MethodImpl::Dex {
            code: Some(code), ..
        } = &m.imp
        {
            let key = std::sync::Arc::as_ptr(code) as usize;
            if seen.insert(key) {
                code_bytes += code.insns.len() as u64 * insn_size;
                code_bytes += code.raw.len() as u64 * 2;
                code_bytes += code.pc_to_idx.len() as u64 * 4;
                code_bytes += code.tries.len() as u64 * 48;
                code_bytes += code.line_table.len() as u64 * 8;
            }
        }
    }
    let class_meta_bytes: u64 = vm
        .classes
        .iter()
        .map(|c| (c.descriptor.len() + c.declared_methods.len() * 8) as u64)
        .sum();
    set_note("vm_dex_bytes", dex_bytes);
    set_note("vm_code_bytes", code_bytes);
    set_note("vm_method_meta_bytes", method_meta_bytes);
    set_note("vm_class_meta_bytes", class_meta_bytes);
    set_note("vm_rss_bytes", rss_bytes());
}


