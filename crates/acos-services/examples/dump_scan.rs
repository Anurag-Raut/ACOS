//! Temporary W125-A helper: print the flattened `writeToParcel` call list.
use acos_services::parcelable::Scanner;

fn main() {
    let dir = std::env::args().nth(1).expect("dex dir");
    for class in std::env::args().skip(2) {
        match Scanner::write_to_parcel(std::path::Path::new(&dir), &class) {
            Ok(s) => {
                println!("=== {class} ({} calls, {} delegates) ===", s.calls.len(), s.delegates.len());
                for (i, c) in s.calls.iter().enumerate() {
                    println!("{i:3} {c}");
                }
                for d in &s.delegates {
                    println!("    delegate: {d}");
                }
            }
            Err(e) => println!("=== {class} ERR {e} ==="),
        }
    }
}
