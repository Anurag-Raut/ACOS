//! W140-A: generic `invoke-polymorphic` `VarHandle` / `MethodHandle` access.
//!
//! The checked-in `varhandle-concurrency.dex` (built from
//! `varhandle-concurrency.java`) exercises the real Android 14 core library's
//! `VarHandle` machinery end to end:
//!
//! * `MethodHandles.lookup().findVarHandle` / `findStaticVarHandle` on `int`,
//!   `long` and reference fields (the real `FieldVarHandle` /
//!   `StaticFieldVarHandle` descriptor path),
//! * `MethodHandles.arrayElementVarHandle(int[].class)`,
//! * `get`/`set`/`getAndSet`/`compareAndSet`/`compareAndExchange`/
//!   `getAndAdd`/`getAndBitwiseOr` and their `Volatile`/`Acquire`/`Release`
//!   variants,
//! * `ConcurrentLinkedQueue` (whose `Node.<init>` was the original
//!   `invoke-polymorphic` blocker), `ConcurrentHashMap`, `Striped64`-backed
//!   `LongAdder` usage via `AtomicLong`, and the `Atomic*FieldUpdater` family
//!   (which drive `Unsafe.objectFieldOffset` and `VarHandle`).
//!
//! It must produce byte-identical stdout to the host JVM under both execution
//! tiers and with GC stress (a collection before every allocation), proving
//! the operand conversion/boxing and root discipline are exact.

use acos_dex::DexFile;
use acos_vm::{CoreLib, Vm, VmOptions};

/// Captured from `java -cp classes Main` (OpenJDK 17, `--release 11`).
const EXPECTED: &str = "\
static int set/get=41 add=41
static long=1099511627776 -> 1099511627781
static ref=hello -> world
inst int=7 long=1234567890123 ref=x
cas=true now=8
cae=8 now=9
vol int=3 -> 7
vol long=true -> 11
vol ref=true -> b
bitwise=7 -> 23
arr=42 cas=true -> 43
holder cas=true -> 5
clq size=100 sum=4950 poll=0
chm size=50 get=7
ai=5 -> 6
al=100 -> 111
aifu=9 -> 14
arfu=true -> y
done
";

fn boot_dir_available() -> bool {
    // `Vm::new` resolves the default boot dir by walking up from the crate.
    acos_vm::vm::default_boot_dir(None)
        .map(|d| d.join("BOOTCLASSPATH").is_file())
        .unwrap_or(false)
}

fn run(ir: bool, gc_stress: bool) -> Result<String, String> {
    acos_vm::exec::set_enabled(Some(ir));
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/varhandle-concurrency.dex"
    ))
    .map_err(|e| e.to_string())?;
    let mut vm = Vm::new(VmOptions {
        heap_max_bytes: 256 * 1024 * 1024,
        gc_stress,
        corelib: CoreLib::Real,
        boot_dir: None,
        trace_natives: false,
    })
    .map_err(|e| format!("{e:?}"))?;
    vm.add_dex(DexFile::parse(bytes).map_err(|e| format!("{e:?}"))?);
    let outcome = vm.run("Main", &[]);
    if outcome.exit_code != 0 {
        return Err(format!(
            "exit {} stderr={}",
            outcome.exit_code, outcome.stderr
        ));
    }
    Ok(outcome.stdout)
}

#[test]
fn varhandle_polymorphic_matches_jvm_both_tiers() {
    if !boot_dir_available() {
        eprintln!("SKIP varhandle_polymorphic_matches_jvm_both_tiers: boot classpath absent");
        return;
    }
    let mut failures = Vec::new();
    for (label, ir, stress) in [
        ("ir/normal", true, false),
        ("ir/gc-stress", true, true),
        ("interp/normal", false, false),
        ("interp/gc-stress", false, true),
    ] {
        match run(ir, stress) {
            Ok(out) if out == EXPECTED => {}
            Ok(out) => failures.push(format!("{label}: stdout mismatch:\n{out}")),
            Err(e) => failures.push(format!("{label}: {e}")),
        }
    }
    acos_vm::exec::set_enabled(None);
    assert!(failures.is_empty(), "{}", failures.join("\n---\n"));
}
