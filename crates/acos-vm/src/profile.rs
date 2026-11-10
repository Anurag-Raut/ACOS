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

/// Charge a byte count to a named allocation call-site category.
///
/// `acos-vm`/`acos-runtime` cannot install a process-wide counting allocator
/// (`#![forbid(unsafe_code)]`), so the big, named buffers instrument their own
/// construction. Counts accumulate; the report's `allocations[]` list is sorted
/// by bytes so the dominant call-site is obvious. Purely diagnostic — never
/// changes behaviour.
pub fn note_alloc(category: &str, bytes: u64) {
    if !enabled() || bytes == 0 {
        return;
    }
    let map = alloc_notes();
    if let Ok(mut m) = map.lock() {
        let e = m.entry(category.to_string()).or_insert((0, 0));
        e.0 += bytes;
        e.1 += 1;
    }
}

/// Reset the accumulated call-site allocation counters.
pub fn reset_allocs() {
    if let Ok(mut m) = alloc_notes().lock() {
        m.clear();
    }
}

/// Record one row of the long-lived-structure census (`ACOS_PROFILE`).
///
/// `name` is a stable bucket label, `bytes` a deep-size estimate and `detail`
/// a short human note (counts). Repeat calls add to the same bucket.
pub fn note_census(name: &str, bytes: u64, detail: &str) {
    if !enabled() || bytes == 0 {
        return;
    }
    let map = census();
    if let Ok(mut m) = map.lock() {
        let e = m.entry(name.to_string()).or_insert((0, String::new()));
        e.0 += bytes;
        if !detail.is_empty() {
            if e.1.is_empty() {
                e.1 = detail.to_string();
            } else if e.1 != detail {
                e.1.push(';');
                e.1.push_str(detail);
            }
        }
    }
}

/// The census, sorted by bytes desc, as `(name, bytes, detail)`.
pub fn census_snapshot() -> Vec<(String, u64, String)> {
    let mut v: Vec<(String, u64, String)> = match census().lock() {
        Ok(g) => g.iter().map(|(k, (b, d))| (k.clone(), *b, d.clone())).collect(),
        Err(_) => Vec::new(),
    };
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

/// Snapshot of `(category -> (bytes, allocations))` sorted by bytes desc.
pub fn alloc_notes_snapshot() -> Vec<(String, u64, u64)> {
    let mut v: Vec<(String, u64, u64)> = match alloc_notes().lock() {
        Ok(g) => g.iter().map(|(k, (b, n))| (k.clone(), *b, *n)).collect(),
        Err(_) => Vec::new(),
    };
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
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
fn alloc_notes() -> &'static Mutex<HashMap<String, (u64, u64)>> {
    static M: OnceLock<Mutex<HashMap<String, (u64, u64)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}
fn census() -> &'static Mutex<HashMap<String, (u64, String)>> {
    static M: OnceLock<Mutex<HashMap<String, (u64, String)>>> = OnceLock::new();
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
        "allocations": alloc_notes_snapshot()
            .into_iter()
            .map(|(name, bytes, count)| json!({"name": name, "bytes": bytes, "count": count}))
            .collect::<Vec<_>>(),
        "census": census_snapshot()
            .into_iter()
            .map(|(name, bytes, detail)| json!({"name": name, "bytes": bytes, "detail": detail}))
            .collect::<Vec<_>>(),
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
    set_note("vm_rss_peak_bytes", rss_peak_bytes());
    note_rss_by_mapping();
    census_vm(vm, method_meta_bytes);
}

/// Peak process resident set size in bytes, from `/proc/self/status` `VmHWM`.
pub fn rss_peak_bytes() -> u64 {
    let Ok(s) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Some(kb) = rest.split_whitespace().next().and_then(|v| v.parse::<u64>().ok()) {
                return kb * 1024;
            }
        }
    }
    0
}

/// Deep-size census of the long-lived Rust structures reachable from one `Vm`
/// plus the process-global DEX caches, emitted behind `ACOS_PROFILE`.
///
/// Every number is a structural byte estimate (lengths/capacities), not an
/// allocator measurement, and the rows are intentionally coarse so they sum to
/// a meaningful share of `RssAnon` without pretending to be exact.
fn census_vm(vm: &Vm, method_meta_bytes: u64) {
    let sz = |n: usize, e: usize| -> u64 { n.saturating_mul(e) as u64 };
    use std::mem::size_of;

    // --- guest heap slot table + GC state ---
    let object_slot = size_of::<Option<crate::heap::Object>>();
    note_census(
        "vm_heap_slots",
        sz(vm.heap.objects.capacity(), object_slot),
        &format!("len={} cap={}", vm.heap.objects.len(), vm.heap.objects.capacity()),
    );
    note_census(
        "vm_heap_gc_bitmap",
        sz(vm.heap.gc_mark.capacity(), 4),
        "mark generation bitmap",
    );
    note_census(
        "vm_heap_gc_stack",
        sz(vm.heap.gc_stack.capacity(), 4),
        "mark worklist",
    );
    note_census(
        "vm_heap_field_pool",
        sz(vm.heap.field_pool.capacity(), size_of::<Vec<u32>>())
            + vm
                .heap
                .field_pool
                .iter()
                .map(|v| sz(v.capacity(), 4))
                .sum::<u64>(),
        &format!("slots={}", vm.heap.field_pool.len()),
    );
    note_census(
        "vm_heap_free_list",
        sz(vm.heap.free.capacity(), 4),
        &format!("len={}", vm.heap.free.len()),
    );

    // --- per-thread Java stacks and the reusable frame pool ---
    let mut frames: u64 = 0;
    let mut frame_n: u64 = 0;
    for t in &vm.threads {
        for f in t.frames.iter().chain(t.frame_pool.iter()) {
            frames += size_of::<crate::frame::Frame>() as u64
                + sz(f.regs.capacity(), 4)
                + sz(f.refs.capacity(), 1);
            frame_n += 1;
        }
    }
    note_census("vm_thread_stacks", frames, &format!("frames={frame_n}"));

    // --- class / method / type metadata (deep) ---
    let mut class_deep: u64 = 0;
    for c in &vm.classes {
        let field_bytes = |v: &[crate::class::FieldDef]| -> u64 {
            v.iter()
                .map(|f| size_of::<crate::class::FieldDef>() as u64 + f.name.len() as u64 + f.desc.len() as u64)
                .sum()
        };
        let map_bytes = |m: &std::collections::HashMap<(String, String), u32>| -> u64 {
            m.iter()
                .map(|((n, s), _)| 16 + n.len() as u64 + s.len() as u64)
                .sum()
        };
        class_deep += c.descriptor.len() as u64
            + c.source_file.as_ref().map_or(0, |s| s.len() as u64 + 24)
            + field_bytes(&c.declared_instance_fields)
            + field_bytes(&c.declared_static_fields)
            + sz(c.instance_refs.capacity(), 1)
            + sz(c.statics.capacity(), 4)
            + sz(c.static_refs.capacity(), 1)
            + sz(c.declared_methods.capacity(), 8)
            + map_bytes(&c.direct_map)
            + map_bytes(&c.virtual_map)
            + sz(c.vtable.capacity(), 8)
            + c.vtable_index
                .keys()
                .map(|(n, s)| 40 + n.len() as u64 + s.len() as u64)
                .sum::<u64>()
            + sz(c.interfaces.capacity(), 4)
            + c.abstract_methods
                .iter()
                .map(|(n, s)| 48 + n.len() as u64 + s.len() as u64)
                .sum::<u64>();
    }
    note_census("vm_class_meta", class_deep, &format!("classes={}", vm.classes.len()));
    note_census(
        "vm_method_meta",
        method_meta_bytes,
        &format!("methods={}", vm.methods.len()),
    );
    note_census(
        "vm_type_meta",
        vm.types
            .iter()
            .map(|t| size_of::<crate::class::TypeInfo>() as u64 + t.descriptor.len() as u64)
            .sum(),
        &format!("types={}", vm.types.len()),
    );
    note_census(
        "vm_class_by_desc",
        vm.class_by_desc
            .keys()
            .map(|k| 48 + k.len() as u64)
            .sum(),
        "descriptor -> ClassId",
    );

    // --- interner / dispatch caches ---
    note_census(
        "vm_string_interner",
        vm.strings
            .keys()
            .map(|k| 24 + k.capacity() as u64 * 2 + 8)
            .sum::<u64>()
            + sz(vm.interned.capacity(), 4),
        &format!("entries={}", vm.strings.len()),
    );
    note_census(
        "vm_dispatch_cache",
        sz(vm.dispatch_cache.capacity(), 24),
        &format!("entries={}", vm.dispatch_cache.len()),
    );
    note_census(
        "vm_ir_quickening",
        sz(vm.ir_fields.capacity(), size_of::<crate::exec::FieldQuick>()),
        &format!("entries={}", vm.ir_fields.len()),
    );
    note_census(
        "vm_native_registry",
        vm.natives
            .keys()
            .map(|(a, b, c)| 72 + a.len() as u64 + b.len() as u64 + c.len() as u64)
            .sum(),
        &format!("entries={}", vm.natives.len()),
    );
    note_census(
        "vm_monitors",
        sz(vm.monitors.capacity(), 16),
        &format!("entries={}", vm.monitors.len()),
    );
    note_census(
        "vm_converters",
        vm.converters
            .values()
            .map(|v| 24 + v.len() as u64 + 8)
            .sum(),
        &format!("entries={}", vm.converters.len()),
    );
    note_census(
        "vm_string_hashes",
        sz(vm.string_hashes.capacity(), 8),
        &format!("entries={}", vm.string_hashes.len()),
    );
    note_census(
        "vm_throwable_stacks",
        vm.throwable_stacks
            .values()
            .map(|v| 40 + sz(v.capacity(), size_of::<crate::vm::StackEntry>()))
            .sum(),
        &format!("entries={}", vm.throwable_stacks.len()),
    );
    note_census("vm_out_buffers", (vm.out_buf.capacity() + vm.err_buf.capacity()) as u64, "stdout+stderr");

    // --- per-VM DEX handle caches ---
    let mut dex_cache_bytes: u64 = 0;
    for d in &vm.dexs {
        dex_cache_bytes += sz(d.string_cache.capacity(), size_of::<Option<crate::heap::Handle>>())
            + sz(d.field_cache.capacity(), size_of::<Option<crate::class::ResolvedField>>())
            + sz(d.method_cache.capacity(), size_of::<Option<crate::class::MethodId>>())
            + sz(d.class_cache.capacity(), size_of::<Option<crate::class::ClassId>>())
            + sz(d.virtual_cache.capacity(), size_of::<Option<crate::vm::VirtualCache>>());
    }
    note_census("vm_dex_handle_caches", dex_cache_bytes, &format!("files={}", vm.dexs.len()));

    // --- DEX raw + parsed tables (deduped across the VM's Arc<DexFile>) ---
    let mut raw: u64 = 0;
    let mut parsed: u64 = 0;
    let mut strings: u64 = 0;
    let mut ann_bytes: u64 = 0;
    let mut ann_count: u64 = 0;
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for d in &vm.dexs {
        let key = std::sync::Arc::as_ptr(&d.file) as usize;
        if !seen.insert(key) {
            continue;
        }
        let f = d.file.footprint();
        raw += f.raw_bytes;
        parsed += f.parsed_bytes;
        strings += f.strings_bytes;
        ann_bytes += f.annotations_bytes;
        ann_count += f.annotations_cached;
    }
    note_census("dex_raw", raw, &format!("files={}", seen.len()));
    note_census("dex_parsed_tables", parsed, "id tables + class defs");
    note_census("dex_decoded_strings", strings, "materialised string tables");
    note_census(
        "dex_annotations",
        ann_bytes,
        &format!("parsed_dirs={ann_count}"),
    );

    // The process-global `BootCache` holds the same `Arc<DexFile>` values
    // counted above, so reporting it separately would double-count the shared
    // DEX bytes. Its (identical) totals are still surfaced as flat notes.
    let g = acos_dex::cache::boot_cache().footprint();
    set_note("dex_bootcache_raw_bytes", g.raw_bytes);
    set_note("dex_bootcache_parsed_bytes", g.parsed_bytes);
    set_note("dex_bootcache_string_bytes", g.strings_bytes);
    set_note("dex_bootcache_annotation_bytes", g.annotations_bytes);
}

/// Roll `RSS` up by mapping category from `/proc/self/smaps` (W111-A).
///
/// The VM's own counters account for the guest heap, raw DEX bytes and decoded
/// code. The remaining RSS is host-side: mmap'd system fonts, the framework
/// resource APK/table, the on-disk boot-image bytes and the allocator's
/// anonymous arenas. This names those categories so the residual is not a
/// single "unaccounted" number. Best-effort: procfs may be unavailable (then
/// no notes are added) and the parser only sums the `Rss:` lines.
#[cfg(target_os = "linux")]
fn note_rss_by_mapping() {
    let Ok(text) = std::fs::read_to_string("/proc/self/smaps") else {
        return;
    };
    let mut fonts: u64 = 0;
    let mut apk: u64 = 0;
    let mut dex_jar: u64 = 0;
    let mut anon: u64 = 0;
    let mut code: u64 = 0;
    let mut other: u64 = 0;
    let mut current = MappingKind::Other;
    for line in text.lines() {
        // A mapping header starts with `start-end` (both hex, 8+ digits).
        let is_header = line.split_whitespace().next().is_some_and(|range| {
            range
                .split_once('-')
                .is_some_and(|(a, b)| {
                    a.len() >= 8
                        && b.len() >= 8
                        && a.bytes().all(|c| c.is_ascii_hexdigit())
                        && b.bytes().all(|c| c.is_ascii_hexdigit())
                })
        });
        if is_header {
            current = classify_mapping(line);
            continue;
        }
        let Some(rest) = line.strip_prefix("Rss:") else {
            continue;
        };
        let kb = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let bytes = kb * 1024;
        match current {
            MappingKind::Fonts => fonts += bytes,
            MappingKind::Apk => apk += bytes,
            MappingKind::DexJar => dex_jar += bytes,
            MappingKind::Anon => anon += bytes,
            MappingKind::Code => code += bytes,
            MappingKind::Other => other += bytes,
        }
    }
    set_note("rss_fonts_bytes", fonts);
    set_note("rss_apk_bytes", apk);
    set_note("rss_dex_jar_bytes", dex_jar);
    set_note("rss_anon_bytes", anon);
    set_note("rss_code_bytes", code);
    set_note("rss_other_bytes", other);
}

#[cfg(not(target_os = "linux"))]
fn note_rss_by_mapping() {}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
enum MappingKind {
    Fonts,
    Apk,
    DexJar,
    Anon,
    Code,
    Other,
}

#[cfg(target_os = "linux")]
fn classify_mapping(header: &str) -> MappingKind {
    // Header form: `start-end perms offset dev inode [path]`.
    let path = header.split_whitespace().nth(5).unwrap_or("");
    let exec = header.split_whitespace().nth(1).is_some_and(|p| p.contains('x'));
    if path.is_empty() || path.starts_with("[anon") || path == "[heap]" || path == "[stack]" {
        MappingKind::Anon
    } else if path.ends_with(".ttf")
        || path.ends_with(".otf")
        || path.ends_with(".ttc")
        || path.contains("/fonts/")
    {
        MappingKind::Fonts
    } else if path.contains("framework-res") || path.ends_with(".apk") {
        MappingKind::Apk
    } else if path.ends_with(".dex")
        || path.ends_with(".jar")
        || path.ends_with(".oat")
        || path.ends_with(".vdex")
        || path.ends_with(".img")
    {
        MappingKind::DexJar
    } else if exec {
        MappingKind::Code
    } else {
        MappingKind::Other
    }
}


