//! T-27 acceptance: the Settings provider (`IContentProvider.call`) served by
//! `acos-services::provider`, driven through the real hand-written
//! `ContentProviderProxy`, with a `MemoryIntArray` (fd-backed) generation
//! counter and PUT-driven generation bumps. Runs under `ACOS_GC_STRESS=1` too.

mod common;

use common::{compile_case, repo_root, run_ok};

#[test]
fn settings_provider_generation_matches_the_golden() {
    let Some(dex) = compile_case(&["SettingsMain"]) else {
        eprintln!("SKIP settings_provider_generation_matches_the_golden: javac/d8/android.jar unavailable");
        return;
    };
    let Some(stdout) = run_ok(&dex, "android.content.SettingsMain", "SettingsMain") else {
        return;
    };
    let expected = std::fs::read_to_string(
        repo_root().join("crates/acos-runtime/tests/data/settings.golden"),
    )
    .expect("committed settings golden file");

    let actual: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    let expected: Vec<&str> = expected.lines().filter(|l| !l.is_empty()).collect();
    if actual != expected {
        panic!(
            "Settings golden mismatch\n--- expected ---\n{}\n--- actual ---\n{}\n",
            expected.join("\n"),
            actual.join("\n")
        );
    }
}
