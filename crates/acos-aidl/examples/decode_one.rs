use acos_aidl::generate::BootScanner;
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let scanner = BootScanner::load(".cache/boot/android-34").expect("boot");
    for name in &args {
        match scanner.decode_interface(name) {
            Some(i) => println!("{}", serde_json::to_string(&i).unwrap()),
            None => println!("MISSING {name}"),
        }
    }
}
