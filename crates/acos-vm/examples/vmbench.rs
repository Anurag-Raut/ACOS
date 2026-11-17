//! Small reproducible benchmark driver for the ACOS DEX interpreter.
//!
//! Usage:
//!   cargo run --release -p acos-vm --example vmbench -- <dex> <main> [repeats]
//!
//! Prints per-run wall time, instruction throughput and the top opcode
//! families (so a profile can be read off without an external profiler).
//! This is deliberately dependency-free; it is the runner used while
//! optimising the interpreter. See `tools/bench-vm.sh` for the full JSON
//! benchmark that also compares against the host JVM.

use std::time::Instant;

use acos_dex::DexFile;
use acos_vm::{Vm, VmOptions};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: {} <dex> <main> [repeats] [--ops]", args[0]);
        std::process::exit(2);
    }
    let dex_path = &args[1];
    let main_class = &args[2];
    let repeats: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
    let show_ops = args.iter().any(|a| a == "--ops");

    let bytes = std::fs::read(dex_path).expect("read dex");

    let mut best = f64::MAX;
    let mut last_metrics = None;
    let mut last_ops: Vec<u64> = Vec::new();
    for r in 0..repeats {
        let mut vm = Vm::new(VmOptions {
            heap_max_bytes: 256 * 1024 * 1024,
            gc_stress: false,
            ..VmOptions::default()
        })
        .expect("vm");
        vm.count_opcodes = show_ops && r == repeats - 1;
        vm.add_dex(DexFile::parse(bytes.clone()).expect("parse dex"));
        let t0 = Instant::now();
        let outcome = vm.run(main_class, &[]);
        let dt = t0.elapsed().as_secs_f64() * 1000.0;
        if outcome.exit_code != 0 {
            eprintln!(
                "vmbench: {} exit={} stderr={}",
                dex_path, outcome.exit_code, outcome.stderr
            );
        }
        best = best.min(dt);
        last_metrics = Some(outcome.metrics);
        last_ops = vm.opcode_counts.to_vec();
    }
    let m = last_metrics.unwrap();
    let mips = if best > 0.0 {
        m.instructions as f64 / (best / 1000.0) / 1e6
    } else {
        0.0
    };
    println!(
        "{:<20} best={:8.2} ms  insns={:>13}  {:6.2} M/s  invokes={:>10}  allocs={:>10}  gc={} gc_ms={} peak_mb={:.1}",
        main_class,
        best,
        m.instructions,
        mips,
        m.invokes,
        m.allocations,
        m.gc_count,
        m.gc_ms,
        m.heap_peak_bytes as f64 / (1024.0 * 1024.0)
    );
    if show_ops {
        let mut ops: Vec<(usize, u64)> = last_ops
            .iter()
            .enumerate()
            .filter(|(_, &c)| c > 0)
            .map(|(i, &c)| (i, c))
            .collect();
        ops.sort_by_key(|&(_, c)| std::cmp::Reverse(c));
        let total: u64 = last_ops.iter().sum();
        for (op, c) in ops.into_iter().take(18) {
            let name = acos_dex::Opcode::from_u8(op as u8)
                .map(|o| o.mnemonic())
                .unwrap_or("unused");
            println!(
                "    {:<14} 0x{:02x} {:>12}  {:5.2}%",
                name,
                op,
                c,
                100.0 * c as f64 / total as f64
            );
        }
    }
}
