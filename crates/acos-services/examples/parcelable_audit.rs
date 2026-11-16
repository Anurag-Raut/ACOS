//! Print every parcelable codec beside the image's own `writeToParcel` calls.
//!
//! ```text
//! cargo run -p acos-services --example parcelable-audit -- \
//!     .cache/boot/android-34/dex/framework
//! ```
use acos_services::parcelable;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::path::Path::new(&args[0]);
    for spec in parcelable::SPECS {
        let scanned = parcelable::Scanner::write_to_parcel(dir, spec.class);
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
