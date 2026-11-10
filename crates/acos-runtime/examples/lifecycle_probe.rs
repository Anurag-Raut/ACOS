//! Manual M2 lifecycle probe: boot the real framework, deliver
//! `bindApplication` + the launch transaction, run the main looper, and print
//! the observed lifecycle log plus the first structured blocker.
//!
//! ```sh
//! cargo run -q -p acos-runtime --example lifecycle_probe -- fixtures/build/apps/hello-views.apk
//! ```

use std::path::PathBuf;

use acos_runtime::Runtime;
use acos_vm::{CoreLib, VmOptions};

fn main() {
    let apk: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "fixtures/build/apps/hello-views.apk".to_string())
        .into();
    eprintln!("lifecycle probe: apk={}", apk.display());

    let mut rt = Runtime::standard_with(VmOptions {
        corelib: CoreLib::Real,
        trace_natives: true,
        ..VmOptions::default()
    })
    .expect("runtime");

    let report = rt.boot_and_run_lifecycle(&apk);
    eprintln!("lifecycle in {:?}", report.elapsed);
    eprintln!(
        "  bind_delivered={} app_created={} tx_scheduled={} tx_delivered={} looper_quit={}",
        report.bind_delivered,
        report.app_created,
        report.transaction_scheduled,
        report.transaction_delivered,
        report.looper_quit
    );
    eprintln!("  classes={}", report.classes_loaded);
    eprintln!("  events:");
    for e in &report.events {
        eprintln!("    {e}");
    }
    if let Some(err) = &report.error {
        eprintln!("  ERROR: {err}");
        std::process::exit(1);
    }
}
