//! `acos bench sessions-shared` — per-session memory with process-shared boot
//! data.
//!
//! This is the companion of `sessions` (BENCHMARKS.md §8): it boots N
//! independent sessions (separate heaps, one process), then reports
//!
//! * total `anon+shmem` / PSS / private / shared pages from
//!   `/proc/self/smaps_rollup` and `/proc/self/status`,
//! * incremental `(total - bare) / N` and marginal `(total(high) - total(low)) /
//!   (high - low)`,
//! * a **structural footprint** of one booted VM: raw boot-DEX bytes, the
//!   parsed `DexFile` tables, the per-VM DEX caches, decoded method bodies
//!   (shared), the class/method/type tables, the interner, the guest heap, the
//!   thread stacks and the small maps,
//! * how much the process-shared caches saved: the same structural walk is run
//!   once with the process-global dedup sets (what the process actually holds)
//!   and once treating every session as independent (what N unshared copies
//!   would cost).
//!
//! # What is shared, and what is not
//!
//! | data | owner | sharing |
//! |---|---|---|
//! | boot-classpath raw DEX bytes | `acos_dex::cache::BootCache` | one `Arc<[u8]>` per file, process-wide |
//! | parsed `DexFile` id tables + lazy string table + class index | `acos_dex::cache::BootCache` | one parsed file per fingerprint, process-wide |
//! | decoded method bodies (`MethodCode`) | `acos_vm::codecache` | one body per `(DexFile::id, code_off)` |
//! | `Vm` class table, methods, types, statics, interner, heap, stacks | each `Vm` | **not shared** (separate heaps) |
//!
//! The accounting is exact for the byte lengths it sums (`Vec::capacity()`,
//! `String::capacity()`, `DexFile` slice lengths, `MethodCode` buffers). The
//! `*_lookup_maps` and `natives_misc` rows add a one-byte-per-entry control
//! estimate to `HashMap`/`HashSet` entries and are labelled as such: they are
//! **not** allocator-exact, and the process-level `/proc` numbers are the
//! cross-check, not the sum of this table.

use std::collections::HashSet;
use std::sync::Arc;

use acos_dex::cache::boot_cache;
use acos_dex::DexFile;
use acos_vm::class::{Class, FieldDef, Method, MethodCode, MethodImpl, TypeInfo};
use acos_vm::frame::Frame;
use acos_vm::vm::{DexCtx, Vm};
use acos_vm::Handle;
use serde_json::{json, Value};

use super::bench::{
    boot_only, build_runtime, file_sha256, kb_value, kib, parse_session_counts, read_memory,
    resolve_apk, round3, settle, spawn_child, stats, BenchArgs, BenchFailure,
};

// ---------------------------------------------------------------------------
// metric: sessions-shared
// ---------------------------------------------------------------------------

pub(crate) fn run(args: &BenchArgs) -> Result<Value, BenchFailure> {
    let apk = resolve_apk(args)?;
    // This metric is the one that scales to N=20: when the caller leaves the
    // shared `--sessions` default (1,2,5,10), extend it with 20 so the
    // long-run marginal is measured. An explicit `--sessions` is respected.
    let spec = if args.sessions.trim() == "1,2,5,10" {
        "1,2,5,10,20".to_string()
    } else {
        args.sessions.clone()
    };
    let counts = parse_session_counts(&spec)?;
    // Memory metrics need fresh-core pairs (BENCHMARKS.md §2 rule 2).
    let probes = args.runs.clamp(3, 5);
    let mut per_n: Vec<(usize, Value)> = Vec::new();

    for n in &counts {
        let mut pss = Vec::new();
        let mut anon_shmem = Vec::new();
        let mut private_kib = Vec::new();
        let mut shared_kib = Vec::new();
        let mut file_kib = Vec::new();
        let mut footprint: Value = Value::Null;
        let mut shared_units: Value = Value::Null;
        let mut raw: Option<Value> = None;

        for _ in 0..probes {
            let probe = spawn_child(&[
                "bench",
                "sessions-shared",
                "--child",
                "sessions-shared",
                "--n",
                &n.to_string(),
                "--corelib",
                &args.corelib,
                "--apk",
                apk.to_string_lossy().as_ref(),
                "--settle-ms",
                &args.settle_ms.to_string(),
            ])?;
            let mem = probe.get("memory").ok_or_else(|| {
                BenchFailure::internal("sessions-shared probe returned no memory block")
            })?;
            pss.push(kib(mem, "pss_kib"));
            anon_shmem.push(kib(mem, "anon_plus_shmem_kib"));
            private_kib.push(kib(mem, "private_kib"));
            shared_kib.push(kib(mem, "shared_kib"));
            file_kib.push(kib(mem, "rss_file_kib"));
            if let Some(fp) = probe.get("footprint") {
                footprint = fp.clone();
            }
            if let Some(units) = probe.get("shared_units") {
                shared_units = units.clone();
            }
            raw = Some(mem.clone());
        }

        per_n.push((
            *n,
            json!({
                "n": n,
                "samples": pss.len(),
                "pss_kib": stats(&pss),
                "anon_plus_shmem_kib": stats(&anon_shmem),
                "private_kib": stats(&private_kib),
                "shared_kib": stats(&shared_kib),
                "rss_file_kib": stats(&file_kib),
                "miB_anon_plus_shmem_p50": round3(median(&anon_shmem) / 1024.0),
                "footprint": footprint,
                "shared_units": shared_units,
                "raw": raw,
            }),
        ));
    }

    let bare = per_n
        .iter()
        .find(|(n, _)| *n == 0)
        .map(|(_, v)| kb_value(v, "anon_plus_shmem_kib"));
    let mut incremental = Vec::new();
    for (n, value) in &per_n {
        if *n == 0 {
            continue;
        }
        let total = kb_value(value, "anon_plus_shmem_kib");
        let per_session = match bare {
            Some(b) if *n > 0 => (total - b) / (*n as f64),
            _ => total / (*n as f64),
        };
        incremental.push(json!({
            "n": n,
            "incremental_anon_plus_shmem_kib_per_session": round3(per_session),
            "incremental_miB_per_session": round3(per_session / 1024.0),
        }));
    }
    let mut marginal = Vec::new();
    for pair in per_n.windows(2) {
        let (low_n, low) = &pair[0];
        let (high_n, high) = &pair[1];
        if high_n == low_n {
            continue;
        }
        let dh = (*high_n - *low_n) as f64;
        let delta = kb_value(high, "anon_plus_shmem_kib") - kb_value(low, "anon_plus_shmem_kib");
        marginal.push(json!({
            "from_n": low_n,
            "to_n": high_n,
            "marginal_anon_plus_shmem_kib_per_session": round3(delta / dh),
            "marginal_miB_per_session": round3(delta / dh / 1024.0),
        }));
    }

    let last = per_n.last().map(|(_, v)| v.clone()).unwrap_or(Value::Null);
    Ok(json!({
        "metric": "sessions-shared",
        "definition": "total + per-session incremental/marginal memory of N independent \
                       in-process sessions that share the immutable boot classpath (raw DEX, \
                       parsed DexFile, decoded method bodies) through process-global caches",
        "unit": "KiB (MiB convenience columns); structural footprint in bytes",
        "type": "anon+shmem (RssAnon+RssShmem) with PSS and private/shared reported separately",
        "cold_or_warm": "fresh process per N; sessions booted sequentially and all kept alive",
        "corelib": args.corelib,
        "fixture": apk.display().to_string(),
        "apk_sha256": file_sha256(&apk),
        "sessions": per_n.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
        "incremental": incremental,
        "marginal": marginal,
        "sharing": {
            "raw_dex": "one Arc<[u8]> + parsed DexFile per boot file per process (acos_dex::cache)",
            "decoded_code": "one MethodCode per (DexFile::id, code_off) per process (acos_vm::codecache)",
            "per_session": "class table, statics, types, interner, guest heap, stacks (separate heaps)",
            "not_shared": "nothing mutable is shared: no handles, class ids, statics or heap slots cross a Vm",
            "last_n_footprint": last,
        },
        "limit": "Runtime::with_host installs the process-global Binder transport, so N live runtimes \
                  are a memory experiment for trusted agent-owned workloads, not N isolated untrusted \
                  tenants. Processes remain the isolation unit; see results/bench/MEMORY_PROFILE.md",
    }))
}

// ---------------------------------------------------------------------------
// child probe: boot N sessions in one process and measure
// ---------------------------------------------------------------------------

pub(crate) fn child_body(args: &BenchArgs) -> Result<Value, BenchFailure> {
    let mut runtimes = Vec::new();
    for _ in 0..args.n {
        let mut rt = build_runtime(&args.corelib)?;
        boot_only(&mut rt, &args.apk)?;
        runtimes.push(rt);
    }
    settle(args.settle_ms);
    let memory = read_memory()
        .ok_or_else(|| BenchFailure::internal("could not read /proc/self/smaps_rollup"))?;
    let footprint = footprint(&runtimes);
    let cache = boot_cache().stats();
    let shared_units = json!({
        "boot_dex_files_resident": cache.entries,
        "boot_dex_cache_hits": cache.hits,
        "boot_dex_cache_loads": cache.loads,
        "decoded_method_bodies_resident": acos_vm::codecache::len(),
    });
    drop(runtimes);
    Ok(json!({
        "mode": "sessions-shared",
        "n": args.n,
        "memory": memory,
        "footprint": footprint,
        "shared_units": shared_units,
    }))
}

// ---------------------------------------------------------------------------
// structural footprint
// ---------------------------------------------------------------------------

/// Owned bytes of one VM's structures, split by where they live.
#[derive(Default, Clone, Copy)]
struct Bytes {
    boot_dex_raw: usize,
    dex_parsed: usize,
    dex_per_vm_caches: usize,
    method_code: usize,
    method_code_slots: usize,
    class_table: usize,
    lookup_maps: usize,
    methods_table: usize,
    types_table: usize,
    interner: usize,
    guest_heap: usize,
    stacks: usize,
    natives_misc: usize,
}

impl Bytes {
    fn add(&mut self, o: &Bytes) {
        self.boot_dex_raw += o.boot_dex_raw;
        self.dex_parsed += o.dex_parsed;
        self.dex_per_vm_caches += o.dex_per_vm_caches;
        self.method_code += o.method_code;
        self.method_code_slots += o.method_code_slots;
        self.class_table += o.class_table;
        self.lookup_maps += o.lookup_maps;
        self.methods_table += o.methods_table;
        self.types_table += o.types_table;
        self.interner += o.interner;
        self.guest_heap += o.guest_heap;
        self.stacks += o.stacks;
        self.natives_misc += o.natives_misc;
    }

    fn total(&self) -> usize {
        self.boot_dex_raw
            + self.dex_parsed
            + self.dex_per_vm_caches
            + self.method_code
            + self.method_code_slots
            + self.class_table
            + self.lookup_maps
            + self.methods_table
            + self.types_table
            + self.interner
            + self.guest_heap
            + self.stacks
            + self.natives_misc
    }

    fn to_json(self, sessions: usize) -> Value {
        let sessions = sessions.max(1);
        let mib = |bytes: usize| round3(bytes as f64 / (1024.0 * 1024.0));
        json!({
            "sessions": sessions,
            "boot_dex_raw_bytes": self.boot_dex_raw,
            "dex_parsed_shared_bytes": self.dex_parsed,
            "dex_per_vm_caches_bytes": self.dex_per_vm_caches,
            "method_code_shared_bytes": self.method_code,
            "method_code_slots_bytes": self.method_code_slots,
            "class_table_bytes": self.class_table,
            "lookup_maps_bytes_approx": self.lookup_maps,
            "methods_table_bytes": self.methods_table,
            "types_table_bytes": self.types_table,
            "interner_bytes": self.interner,
            "guest_heap_bytes": self.guest_heap,
            "stacks_bytes": self.stacks,
            "natives_and_misc_bytes_approx": self.natives_misc,
            "total_owned_bytes": self.total(),
            "total_owned_mib": mib(self.total()),
            "total_owned_mib_per_session": round3(self.total() as f64 / (1024.0 * 1024.0) / sessions as f64),
            "miB": {
                "boot_dex_raw": mib(self.boot_dex_raw),
                "dex_parsed_shared": mib(self.dex_parsed),
                "dex_per_vm_caches": mib(self.dex_per_vm_caches),
                "method_code_shared": mib(self.method_code),
                "class_table": mib(self.class_table),
                "lookup_maps_approx": mib(self.lookup_maps),
                "methods_table": mib(self.methods_table),
                "types_table": mib(self.types_table),
                "interner": mib(self.interner),
                "guest_heap": mib(self.guest_heap),
                "stacks": mib(self.stacks),
                "natives_and_misc_approx": mib(self.natives_misc),
            },
        })
    }
}

fn footprint(runtimes: &[acos_runtime::Runtime]) -> Value {
    let mut seen_dex: HashSet<u64> = HashSet::new();
    let mut seen_code: HashSet<usize> = HashSet::new();
    let mut shared = Bytes::default();
    let mut unshared = Bytes::default();
    for rt in runtimes {
        shared.add(&account(rt.vm(), Some(&mut seen_dex), Some(&mut seen_code)));
        // "Equivalent if nothing were shared": fresh dedup sets per session.
        unshared.add(&account(rt.vm(), None, None));
    }
    let saved = unshared.total().saturating_sub(shared.total());
    json!({
        "what_the_process_actually_holds": shared.to_json(runtimes.len()),
        "unshared_equivalent": unshared.to_json(runtimes.len()),
        "saved_by_sharing_bytes": saved,
        "saved_by_sharing_mib": round3(saved as f64 / (1024.0 * 1024.0)),
        "saved_per_session_mib": round3(
            saved as f64 / (1024.0 * 1024.0) / runtimes.len().max(1) as f64
        ),
        "method": "exact Vec/String capacities + DexFile slice lengths; lookup_maps/natives rows \
                   carry a one-byte-per-entry control estimate and are marked _approx",
    })
}

fn account(
    vm: &Vm,
    mut seen_dex: Option<&mut HashSet<u64>>,
    mut seen_code: Option<&mut HashSet<usize>>,
) -> Bytes {
    let mut b = Bytes::default();

    for ctx in &vm.dexs {
        let file = &ctx.file;
        let first = match &mut seen_dex {
            Some(set) => set.insert(file.id()),
            None => true,
        };
        if first {
            b.boot_dex_raw += file.bytes().len();
            b.dex_parsed += dex_parsed_bytes(file);
        }
        b.dex_per_vm_caches += dex_cache_bytes(ctx);
    }

    for method in &vm.methods {
        if let MethodImpl::Dex {
            code: Some(code), ..
        } = &method.imp
        {
            b.method_code_slots += std::mem::size_of_val(code);
            let ptr = Arc::as_ptr(code) as usize;
            let first = match &mut seen_code {
                Some(set) => set.insert(ptr),
                None => true,
            };
            if first {
                b.method_code += method_code_bytes(code);
            }
        }
    }

    b.class_table = class_bytes(&vm.classes);
    b.lookup_maps = lookup_map_bytes(vm);
    b.methods_table = method_table_bytes(&vm.methods);
    b.types_table = type_table_bytes(&vm.types);
    b.interner = interner_bytes(vm);
    b.guest_heap = vm.heap.live_bytes;
    b.stacks = stack_bytes(vm);
    b.natives_misc = misc_bytes(vm);
    b
}

fn dex_parsed_bytes(file: &DexFile) -> usize {
    let mut n = std::mem::size_of::<DexFile>();
    n += std::mem::size_of_val(file.string_data_offsets());
    n += std::mem::size_of_val(file.type_ids());
    n += std::mem::size_of_val(file.proto_ids());
    n += std::mem::size_of_val(file.field_ids());
    n += std::mem::size_of_val(file.method_ids());
    n += std::mem::size_of_val(file.class_defs());
    n += std::mem::size_of_val(file.map());
    n += std::mem::size_of_val(file.method_handles());
    n += std::mem::size_of_val(file.call_site_ids());
    // Materialised string table (decoded on first class lookup).
    for s in file.strings() {
        n += s.capacity();
    }
    // Lazily-built compact class index (`hash -> def_idx` entries + buckets).
    n += file.class_index().len() * (std::mem::size_of::<u64>() + std::mem::size_of::<usize>() + 1);
    n
}

fn dex_cache_bytes(ctx: &DexCtx) -> usize {
    let handle = std::mem::size_of::<Option<Handle>>();
    let field = std::mem::size_of::<Option<acos_vm::class::ResolvedField>>();
    let slot = std::mem::size_of::<Option<u32>>();
    ctx.string_cache.capacity() * handle
        + ctx.field_cache.capacity() * field
        + ctx.method_cache.capacity() * slot
        + ctx.class_cache.capacity() * slot
}

fn method_code_bytes(code: &MethodCode) -> usize {
    let mut n = std::mem::size_of::<MethodCode>();
    n += code.insns.capacity() * std::mem::size_of::<acos_vm::class::Insn>();
    n += code.pc_to_idx.capacity() * std::mem::size_of::<u32>();
    n += code.raw.capacity() * std::mem::size_of::<u16>();
    n += code.tries.capacity() * std::mem::size_of::<acos_vm::class::TryEntry>();
    for t in &code.tries {
        n += t.typed.capacity() * std::mem::size_of::<(String, u32)>();
        for (desc, _) in &t.typed {
            n += desc.capacity();
        }
    }
    n += code.line_table.capacity() * std::mem::size_of::<(u32, u32)>();
    n += code.source_file.as_ref().map(|s| s.capacity()).unwrap_or(0);
    n
}

fn class_bytes(classes: &[Class]) -> usize {
    let mut n = std::mem::size_of_val(classes);
    for c in classes {
        n += c.descriptor.capacity();
        n += c.source_file.as_ref().map(|s| s.capacity()).unwrap_or(0);
        n += c.interfaces.capacity() * std::mem::size_of::<u32>();
        n += c.instance_refs.capacity();
        n += c.statics.capacity() * std::mem::size_of::<u32>();
        n += c.static_refs.capacity();
        n += c.declared_methods.capacity() * std::mem::size_of::<u32>();
        n += c.vtable.capacity() * std::mem::size_of::<u32>();
        n += field_bytes(&c.declared_instance_fields);
        n += field_bytes(&c.declared_static_fields);
        for (k, v) in &c.direct_map {
            n += k.0.capacity() + k.1.capacity() + std::mem::size_of_val(v) + 1;
        }
        for (k, v) in &c.virtual_map {
            n += k.0.capacity() + k.1.capacity() + std::mem::size_of_val(v) + 1;
        }
        for (k, v) in &c.vtable_index {
            n += k.0.capacity() + k.1.capacity() + std::mem::size_of_val(v) + 1;
        }
    }
    n
}

fn field_bytes(fields: &[FieldDef]) -> usize {
    let mut n = std::mem::size_of_val(fields);
    for f in fields {
        n += f.name.capacity() + f.desc.capacity();
    }
    n
}

fn method_table_bytes(methods: &[Method]) -> usize {
    let mut n = std::mem::size_of_val(methods);
    for m in methods {
        n += m.name.capacity() + m.sig.capacity();
        n += m.params.capacity() * std::mem::size_of::<u32>();
        n += m.ins_ref.capacity();
    }
    n
}

fn type_table_bytes(types: &[TypeInfo]) -> usize {
    let mut n = std::mem::size_of_val(types);
    for t in types {
        n += t.descriptor.capacity();
    }
    n
}

fn lookup_map_bytes(vm: &Vm) -> usize {
    let mut n = 0;
    for (k, v) in &vm.class_by_desc {
        n += k.capacity() + std::mem::size_of_val(v) + 1;
    }
    for (k, v) in &vm.type_by_desc {
        n += k.capacity() + std::mem::size_of_val(v) + 1;
    }
    n
}

fn interner_bytes(vm: &Vm) -> usize {
    let mut n = std::mem::size_of::<std::collections::HashMap<Vec<u16>, Handle>>();
    for (key, handle) in &vm.strings {
        n += key.capacity() * std::mem::size_of::<u16>();
        n += std::mem::size_of_val(handle) + 1;
    }
    n += vm.interned.capacity() * std::mem::size_of::<Handle>();
    n
}

fn stack_bytes(vm: &Vm) -> usize {
    let mut n = std::mem::size_of_val(vm.threads.as_slice());
    for t in &vm.threads {
        n += t.frames.capacity() * std::mem::size_of::<Frame>();
        n += t.joiners.capacity() * std::mem::size_of::<usize>();
        for f in t.frames.iter() {
            n += f.regs.capacity() * std::mem::size_of::<u32>();
            n += f.refs.capacity();
        }
    }
    n
}

fn misc_bytes(vm: &Vm) -> usize {
    let mut n = std::mem::size_of::<Vm>();
    for ((class, name, sig), f) in &vm.natives {
        n += class.capacity() + name.capacity() + sig.capacity();
        n += std::mem::size_of_val(f) + 1;
    }
    n += vm.monitors.len()
        * (std::mem::size_of::<Handle>() + std::mem::size_of::<(usize, u32)>() + 1);
    n += vm.int_cache.len() * (std::mem::size_of::<i32>() + std::mem::size_of::<Handle>() + 1);
    n += vm.converters.len() * (std::mem::size_of::<u64>() + std::mem::size_of::<String>() + 8);
    n += vm.string_hashes.len() * (std::mem::size_of::<Handle>() + std::mem::size_of::<i32>() + 1);
    n += vm.throwable_stacks.len()
        * (std::mem::size_of::<Handle>() + 2 * std::mem::size_of::<usize>() + 1);
    n += vm.native_roots.capacity() * std::mem::size_of::<Handle>();
    n += vm.builtin_alloc.len() * (std::mem::size_of::<u32>() + std::mem::size_of::<usize>() + 1);
    n += vm.threads.len() * std::mem::size_of::<acos_vm::vm::ThreadRT>();
    n
}

fn median(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = (0.5 * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1]
}
