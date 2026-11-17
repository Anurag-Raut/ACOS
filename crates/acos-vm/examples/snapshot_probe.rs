//! T-42 measurement probe: snapshot a booted VM, restore it, and compare a
//! program run through restore against a fresh VM.
//!
//! ```text
//! cargo run --release -p acos-vm --example snapshot_probe -- \
//!     --corelib bootstrap --dex fixtures/build/dexprogs/01_hello/classes.dex
//! ```
//!
//! It prints a JSON object with the measured numbers used by
//! `results/snapshot/REPORT.md`: snapshot size, fresh `Vm::new` + first-run
//! time, restore decode time (p50/p95), the raw memcpy lower bound a CoW arena
//! would approach, RSS per restored VM, and the determinism/idempotence
//! invariants.
//!
//! This is a measurement tool, not a production path; `acos snapshot` /
//! `acos run-dex --restore` CLI wiring is deliberately left to T-44.

use std::path::PathBuf;
use std::time::Instant;

use acos_dex::DexFile;
use acos_vm::snapshot::RestoreStats;
use acos_vm::vm::{CoreLib, Vm, VmOptions};

#[derive(Clone)]
struct RunResult {
    exit: i32,
    stdout_len: usize,
    stdout: String,
}

fn read_dex(path: &PathBuf) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn run_dex(vm: &mut Vm, dex: &[u8], main: &str) -> RunResult {
    vm.add_dex(DexFile::parse(dex.to_vec()).expect("parse dex"));
    let outcome = vm.run(main, &[]);
    RunResult {
        exit: outcome.exit_code,
        stdout_len: outcome.stdout.len(),
        stdout: outcome.stdout,
    }
}

/// Read the process RSS from `/proc/self/status:VmRSS` (bytes).
fn rss_bytes() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            if let Some(kb) = rest.split_whitespace().next() {
                return kb.parse::<u64>().unwrap_or(0) * 1024;
            }
        }
    }
    0
}

fn percentile(sorted: &[u128], p: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

fn main() {
    let mut corelib = CoreLib::Bootstrap;
    let mut boot_dir: Option<PathBuf> = None;
    let mut dex_path: Option<PathBuf> = None;
    let mut main_class = "Main".to_string();
    let mut iters: usize = 20;
    let mut gc_stress = false;
    let mut rss_vms: usize = 8;

    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--corelib" => {
                i += 1;
                corelib = CoreLib::parse(&args[i]).expect("corelib bootstrap|real");
            }
            "--boot-dir" => {
                i += 1;
                boot_dir = Some(PathBuf::from(&args[i]));
            }
            "--dex" => {
                i += 1;
                dex_path = Some(PathBuf::from(&args[i]));
            }
            "--main" => {
                i += 1;
                main_class = args[i].clone();
            }
            "--iters" => {
                i += 1;
                iters = args[i].parse().expect("iters");
            }
            "--gc-stress" => gc_stress = true,
            "--rss-vms" => {
                i += 1;
                rss_vms = args[i].parse().expect("rss-vms");
            }
            other => panic!("unknown argument {other}"),
        }
        i += 1;
    }
    let dex_path = dex_path.expect("--dex PATH");
    let dex = read_dex(&dex_path);

    let opts = || VmOptions {
        heap_max_bytes: 256 * 1024 * 1024,
        gc_stress,
        corelib,
        boot_dir: boot_dir.clone(),
        trace_natives: false,
    };

    // ---- fresh path ------------------------------------------------------
    let t0 = Instant::now();
    let mut fresh = Vm::new(opts()).expect("fresh Vm::new");
    let vm_new_nanos = t0.elapsed().as_nanos();
    let t1 = Instant::now();
    let fresh_res = run_dex(&mut fresh, &dex, &main_class);
    let fresh_run_nanos = t1.elapsed().as_nanos();
    drop(fresh);

    // ---- template + snapshot --------------------------------------------
    let t2 = Instant::now();
    let template = Vm::new(opts()).expect("template Vm::new");
    let template_new_nanos = t2.elapsed().as_nanos();
    let t3 = Instant::now();
    let snap = template.snapshot().expect("snapshot");
    let snapshot_nanos = t3.elapsed().as_nanos();
    drop(template);

    // ---- restore + run N times ------------------------------------------
    let mut restore_nanos: Vec<u128> = Vec::with_capacity(iters);
    let mut restore_run_nanos: Vec<u128> = Vec::with_capacity(iters);
    let mut copy_nanos: Vec<u128> = Vec::with_capacity(iters);
    let mut identical = 0usize;
    let mut last_diff: Option<String> = None;
    for _ in 0..iters {
        let t = Instant::now();
        let (mut vm, stats): (Vm, RestoreStats) = Vm::restore_with_stats(&snap).expect("restore");
        restore_nanos.push(t.elapsed().as_nanos());
        copy_nanos.push(stats.copy_nanos as u128);
        let tr = Instant::now();
        let r = run_dex(&mut vm, &dex, &main_class);
        restore_run_nanos.push(tr.elapsed().as_nanos());
        if r.exit == fresh_res.exit && r.stdout == fresh_res.stdout {
            identical += 1;
        } else if last_diff.is_none() {
            last_diff = Some(format!(
                "exit {} vs {}, stdout {} vs {} bytes",
                r.exit, fresh_res.exit, r.stdout_len, fresh_res.stdout_len
            ));
        }
    }

    // ---- invariants ------------------------------------------------------
    let det_a = {
        let vm = Vm::new(opts()).expect("vm");
        vm.snapshot().expect("snap")
    };
    let det_b = {
        let vm = Vm::new(opts()).expect("vm");
        vm.snapshot().expect("snap")
    };
    let determinism = det_a == det_b;
    let idempotence = {
        let vm = Vm::restore(&snap).expect("restore for idempotence");
        let snap2 = vm.snapshot().expect("resnapshot");
        snap2 == snap
    };

    // ---- RSS per restored VM --------------------------------------------
    let rss_before = rss_bytes();
    let mut held = Vec::new();
    for _ in 0..rss_vms {
        held.push(Vm::restore(&snap).expect("restore for rss"));
    }
    let rss_after = rss_bytes();
    let per_vm_rss = if rss_vms > 0 {
        rss_after.saturating_sub(rss_before) / rss_vms as u64
    } else {
        0
    };
    drop(held);

    restore_nanos.sort_unstable();
    restore_run_nanos.sort_unstable();
    copy_nanos.sort_unstable();

    let json = format!(
        concat!(
            "{{\n",
            "  \"corelib\": \"{corelib}\",\n",
            "  \"dex\": \"{dex}\",\n",
            "  \"gc_stress\": {gc_stress},\n",
            "  \"iters\": {iters},\n",
            "  \"snapshot_bytes\": {snap_bytes},\n",
            "  \"snapshot_nanos\": {snapshot_nanos},\n",
            "  \"fresh_vm_new_nanos\": {vm_new_nanos},\n",
            "  \"template_vm_new_nanos\": {template_new_nanos},\n",
            "  \"fresh_run_nanos\": {fresh_run_nanos},\n",
            "  \"restore_nanos_p50\": {rp50},\n",
            "  \"restore_nanos_p95\": {rp95},\n",
            "  \"restore_copy_nanos_p50\": {cp50},\n",
            "  \"restore_copy_nanos_p95\": {cp95},\n",
            "  \"restore_then_run_nanos_p50\": {rrp50},\n",
            "  \"restore_then_run_nanos_p95\": {rrp95},\n",
            "  \"identical_outputs\": {identical},\n",
            "  \"diff\": {diff},\n",
            "  \"determinism_byte_equal\": {determinism},\n",
            "  \"idempotence_byte_equal\": {idempotence},\n",
            "  \"rss_per_restored_vm_bytes\": {per_vm_rss}\n",
            "}}\n"
        ),
        corelib = match corelib {
            CoreLib::Bootstrap => "bootstrap",
            CoreLib::Real => "real",
        },
        dex = dex_path.display(),
        gc_stress = gc_stress,
        iters = iters,
        snap_bytes = snap.len(),
        snapshot_nanos = snapshot_nanos,
        vm_new_nanos = vm_new_nanos,
        template_new_nanos = template_new_nanos,
        fresh_run_nanos = fresh_run_nanos,
        rp50 = percentile(&restore_nanos, 0.50),
        rp95 = percentile(&restore_nanos, 0.95),
        cp50 = percentile(&copy_nanos, 0.50),
        cp95 = percentile(&copy_nanos, 0.95),
        rrp50 = percentile(&restore_run_nanos, 0.50),
        rrp95 = percentile(&restore_run_nanos, 0.95),
        identical = identical,
        diff = match &last_diff {
            Some(d) => format!("\"{d}\""),
            None => "null".to_string(),
        },
        determinism = determinism,
        idempotence = idempotence,
        per_vm_rss = per_vm_rss,
    );
    println!("{json}");
}
