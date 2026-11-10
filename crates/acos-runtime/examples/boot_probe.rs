//! Manual M2 boot probe: run the real framework attach path and print the
//! first blocker (used while iterating in `results/m2/BOOT_LOG.md`).
//!
//! ```sh
//! cargo run -q -p acos-runtime --example boot_probe -- fixtures/build/apps/hello-views.apk
//! ```

use std::path::PathBuf;

use acos_runtime::Runtime;
use acos_vm::{CoreLib, VmOptions};

fn main() {
    let apk: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "fixtures/build/apps/hello-views.apk".to_string())
        .into();
    eprintln!("boot probe: apk={}", apk.display());

    let mut rt = Runtime::standard_with(VmOptions {
        corelib: CoreLib::Real,
        trace_natives: true,
        ..VmOptions::default()
    })
    .expect("runtime");

    match rt.boot_app_process(&apk) {
        Ok(report) => {
            eprintln!("BOOT OK in {:?}", report.elapsed);
            eprintln!("  stages: {:?}", report.stages);
            eprintln!("  attach: {:?}", report.attach);
            eprintln!(
                "  classes: {} (boot classpath {})",
                report.classes_loaded, report.boot_classpath_classes
            );
            eprintln!("  natives: {}", report.natives_invoked.len());
            if let Some(path) = std::env::var_os("ACOS_BOOT_REPORT") {
                let natives = report
                    .natives_invoked
                    .iter()
                    .map(|(c, m, s)| format!("\"{c}->{m}{s}\""))
                    .collect::<Vec<_>>()
                    .join(",\n    ");
                let json = format!(
                    "{{\n  \"elapsed_ms\": {:.3},\n  \"classes_loaded\": {},\n  \"boot_classpath_classes\": {},\n  \"attach_binder\": {},\n  \"attach_seq\": {},\n  \"natives_invoked\": {},\n  \"natives\": [\n    {}\n  ]\n}}\n",
                    report.elapsed.as_secs_f64() * 1000.0,
                    report.classes_loaded,
                    report.boot_classpath_classes,
                    report.attach.app_thread.map(|b| b.0 as i64).unwrap_or(-1),
                    report.attach.start_seq,
                    report.natives_invoked.len(),
                    natives,
                );
                std::fs::write(&path, json).expect("write boot report");
                eprintln!("  wrote {}", std::path::Path::new(&path).display());
            }
        }
        Err(e) => {
            eprintln!("BOOT FAILED: {e}");
            let mut src = std::error::Error::source(&e);
            while let Some(s) = src {
                eprintln!("  caused by: {s}");
                src = std::error::Error::source(s);
            }
            std::process::exit(1);
        }
    }
}
