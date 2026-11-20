//! Differential corpus test: every `fixtures/dexprogs` program is compiled to
//! real DEX and its stdout / exit code must match the checked-in
//! `jvm-reference` expectations byte for byte.
//!
//! The test is skipped (loudly) when `fixtures/build` is absent, so the crate
//! still builds and tests on a checkout without the Android SDK.

use std::path::{Path, PathBuf};

use acos_dex::DexFile;
use acos_vm::{Vm, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture_names() -> Vec<String> {
    let dir = repo_root().join("fixtures/build/dexprogs");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn run_one(name: &str, gc_stress: bool, count_opcodes: bool) -> (Vm, i32, String) {
    let dex_path = repo_root().join(format!("fixtures/build/dexprogs/{name}/classes.dex"));
    let bytes = std::fs::read(&dex_path).expect("read dex");
    let options = VmOptions {
        heap_max_bytes: 256 * 1024 * 1024,
        gc_stress,
        ..VmOptions::default()
    };
    let mut vm = Vm::new(options).expect("vm new");
    vm.count_opcodes = count_opcodes;
    vm.add_dex(DexFile::parse(bytes).expect("parse dex"));
    let outcome = vm.run("Main", &[]);
    (vm, outcome.exit_code, outcome.stdout)
}

fn expected(name: &str, ext: &str) -> String {
    let p = repo_root().join(format!("fixtures/expected/dexprogs/{name}.{ext}"));
    // The fixture program exists (this test is only reached for it), so a
    // missing reference file is a hard failure, never an empty stdout / zero
    // exit that would make a silent non-run look like a pass
    // (HONESTY_AUDIT #23).
    std::fs::read_to_string(&p).unwrap_or_else(|e| {
        panic!(
            "missing checked-in reference {}: {e}; the fixture program exists, so a \
             missing expectation must not silently become empty/zero",
            p.display()
        )
    })
}

/// Programs that can only run against the real Android 14 boot classpath.
///
/// The bootstrap corelib does not implement `java.util.regex`; the regex corpus
/// exercises the ICU-backed `Pattern`/`Matcher` (`com.android.icu.util.regex.*`)
/// and is verified against real ART by `tools/diff-dexprogs.sh --corelib real`
/// (expectations in `fixtures/expected-art/dexprogs`).
const REAL_CORELIB_ONLY: &[&str] = &[
    "21_regex_basic",
    "22_regex_advanced",
    "23_regex_split_replace",
    "24_zip_roundtrip",
    "25_security_digest",
    "26_regex_matrix",
    // DEX annotation reflection needs real `libcore.reflect.AnnotationFactory`
    // and `java.lang.reflect.Proxy`; verified against ART by
    // `tools/diff-dexprogs.sh --corelib real`.
    "27_annotations_reflection",
    // Framework-backed (`android.database.sqlite.*`): the bootstrap corelib has
    // no android.database, and the host JVM cannot run it either (its checked-in
    // reference is the JVM's "Unable to initialize main class", exit 1). The
    // real output is compared on ART by
    // `tools/oracle/run-framework-dexprog-art.sh` (fixtures/expected-art).
    // It was invisible to this test only while `build-fixtures.sh` kept
    // deleting its build (MERGEFIX2); with the fixture built it must be named.
    "28_sqlite_basic",
];

#[test]
fn dexprogs_match_jvm_reference() {
    if !acos_test_support::have_fixture("fixtures/build/dexprogs") {
        acos_test_support::require_prereq(false, "dexprogs_match_jvm_reference", "dexprogs_match_jvm_reference: fixtures/build/dexprogs is absent (run tools/build-fixtures.sh)");
        return;
    }
    let names = fixture_names();
    let mut executed = [0u64; 256];
    let mut failures = Vec::new();
    for name in &names {
        if REAL_CORELIB_ONLY.contains(&name.as_str()) {
            continue;
        }
        let (vm, code, stdout) = run_one(name, false, true);
        let want_stdout = expected(name, "stdout");
        let want_exit: i32 = expected(name, "exit").trim().parse().unwrap_or(0);
        if stdout != want_stdout || code != want_exit {
            failures.push(format!(
                "{name}: exit {code} (want {want_exit}), stdout {} bytes (want {} bytes)",
                stdout.len(),
                want_stdout.len()
            ));
        }
        for (i, c) in vm.opcode_counts.iter().enumerate() {
            executed[i] += c;
        }
    }

    // Write the opcode coverage report.
    write_coverage(&names, &executed);

    assert!(
        failures.is_empty(),
        "{} of {} dexprogs failed:\n{}",
        failures.len(),
        names.len(),
        failures.join("\n")
    );
}

/// The corpus must also pass with a collection before every allocation, which
/// proves that every GC root is exact. (The heavy threading program 19 is
/// excluded from the per-allocation stress run to keep the test bounded; it is
/// still covered by the normal pass and by `tools/diff-dexprogs.sh --gc-stress`.)
#[test]
fn dexprogs_pass_gc_stress() {
    if acos_test_support::require_prereq(acos_test_support::have_fixture("fixtures/build/dexprogs"), "dexprogs_pass_gc_stress", "fixtures/build/dexprogs is absent").is_none() {
        return;
    }
    let names = fixture_names();
    let mut failures = Vec::new();
    for name in &names {
        if name == "19_threads_sync" || REAL_CORELIB_ONLY.contains(&name.as_str()) {
            continue;
        }
        let (_vm, code, stdout) = run_one(name, true, false);
        let want_stdout = expected(name, "stdout");
        let want_exit: i32 = expected(name, "exit").trim().parse().unwrap_or(0);
        if stdout != want_stdout || code != want_exit {
            failures.push(format!("{name}: exit {code} (want {want_exit})"));
        }
    }
    assert!(
        failures.is_empty(),
        "GC stress failures:\n{}",
        failures.join("\n")
    );
}

fn write_coverage(names: &[String], executed: &[u64; 256]) {
    let out_dir = acos_test_support::test_artifacts_dir("vm");
    let _ = std::fs::create_dir_all(&out_dir);
    let unsupported: [u8; 6] = [0xfa, 0xfb, 0xfc, 0xfd, 0xfe, 0xff];
    let mut exec = Vec::new();
    let mut unexercised = Vec::new();
    for op in acos_dex::defined_opcodes() {
        let b = op.byte();
        if unsupported.contains(&b) {
            continue;
        }
        if executed[b as usize] > 0 {
            exec.push(serde_json::json!({
                "opcode": format!("0x{b:02x}"),
                "mnemonic": op.mnemonic(),
                "count": executed[b as usize],
            }));
        } else {
            unexercised.push(format!("{} (0x{b:02x})", op.mnemonic()));
        }
    }
    let doc = serde_json::json!({
        "corpus": names,
        "executed": exec,
        "implemented_but_unexercised": unexercised,
        "unsupported": [
            {"opcode": "0xfa", "mnemonic": "invoke-polymorphic"},
            {"opcode": "0xfb", "mnemonic": "invoke-polymorphic/range"},
            {"opcode": "0xfc", "mnemonic": "invoke-custom"},
            {"opcode": "0xfd", "mnemonic": "invoke-custom/range"},
            {"opcode": "0xfe", "mnemonic": "const-method-handle"},
            {"opcode": "0xff", "mnemonic": "const-method-type"},
        ],
    });
    let path = out_dir.join("opcode-coverage.json");
    if let Ok(s) = serde_json::to_string_pretty(&doc) {
        let _ = std::fs::write(path, s);
    }
}
