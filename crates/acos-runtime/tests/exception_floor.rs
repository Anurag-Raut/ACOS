//! Real-corelib regression for the exception floor and ART's
//! `ExceptionInInitializerError` / `NoClassDefFoundError` semantics, including
//! the native-initiated `Class.forName` path (`classForName` native ->
//! `initialize_class`). A `<clinit>` failure there must stop at the native's
//! Rust base and be rethrown into the outer Java handler, not unwind past it
//! with a stale frame index.

mod common;

use common::{compile_case, run_ok};

#[test]
fn exception_floor_rethrows_native_init_failure() {
    let Some(dex) = compile_case(&["ExceptionFloorMain"]) else {
        acos_test_support::require_prereq(false, "exception_floor_rethrows_native_init_failure", "exception_floor_rethrows_native_init_failure: javac/d8/android.jar unavailable");
        return;
    };
    let Some(stdout) = run_ok(&dex, "ExceptionFloorMain", "ExceptionFloorMain") else {
        return;
    };
    assert_eq!(stdout, "eie:boom\nncdfe\nforname-eie:boom2\nncdfe2\ndone\n");
}
