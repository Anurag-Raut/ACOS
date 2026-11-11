//! Print the collapsed `Parcel.write*` sequence [`parcelable::verify`] sees for a
//! `writeToParcel`, which is what a `ParcelableSpec` must match.
//!
//! ```text
//! cargo run -p acos-services --example scan-write-calls -- \
//!     .cache/boot/android-34/dex/framework android.view.DisplayInfo
//! ```

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: scan-write-calls <dex-dir> <class> [class ...]");
        std::process::exit(2);
    }
    let dir = std::path::Path::new(&args[0]);
    for class in &args[1..] {
        match acos_services::parcelable::Scanner::write_to_parcel(dir, class) {
            Ok(scanned) => {
                println!("=== {class} ({} calls) ===", scanned.calls.len());
                for (i, call) in scanned.calls.iter().enumerate() {
                    println!("{i:>3}  {call}");
                }
                for d in &scanned.delegates {
                    println!("     delegate {d}");
                }
            }
            Err(e) => println!("=== {class} ERROR {e} ==="),
        }
    }
}
