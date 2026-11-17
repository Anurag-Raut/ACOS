//! Print every parcelable codec beside the image's own `writeToParcel` calls.
//!
//! With no arguments the oracle scans **every** boot-classpath DEX directory of
//! the image (`BOOTCLASSPATH`, `SYSTEMSERVERCLASSPATH` and every other jar the
//! image ships), which is what the crate test uses:
//!
//! ```text
//! cargo run -p acos-services --example parcelable_audit
//! ```
//!
//! Explicit directories restrict the scan (useful to diff one jar):
//!
//! ```text
//! cargo run -p acos-services --example parcelable_audit -- \
//!     .cache/boot/android-34/dex/framework-connectivity
//! ```
use acos_services::parcelable;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dirs = if args.is_empty() {
        parcelable::boot_classpath_dex_dirs().expect("boot DEX cache (run from the repo root)")
    } else {
        args.iter().map(std::path::PathBuf::from).collect()
    };
    println!(
        "## scanning {} boot-classpath DEX dirs: {}",
        dirs.len(),
        dirs.iter()
            .map(|d| d
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!();
    for spec in parcelable::SPECS {
        let scanned = parcelable::Scanner::write_to_parcel_in(&dirs, spec.class);
        match scanned {
            Ok(scanned) => {
                let v = parcelable::verify(spec, &scanned);
                println!(
                    "### {} | {:?} | matched {}/{} | full={}",
                    spec.class,
                    spec.verification,
                    v.matched_calls,
                    v.real_calls,
                    v.is_full()
                );
                println!("ACOS:");
                for f in spec.fields {
                    println!("  {f:?}");
                }
                println!("IMAGE:");
                for c in &scanned.calls {
                    println!("  {c}");
                }
                for d in &scanned.delegates {
                    println!("  delegate {d}");
                }
            }
            Err(e) => {
                println!(
                    "### {} | {:?} | no writeToParcel ({e})",
                    spec.class, spec.verification
                );
                println!("ACOS:");
                for f in spec.fields {
                    println!("  {f:?}");
                }
            }
        }
        println!();
    }
}
