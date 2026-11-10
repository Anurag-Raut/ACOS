//! Builds the small C support objects (variadic shims and stdio translation).
//! See `csrc/guest_shims.c` for why C is required.

fn main() {
    println!("cargo:rerun-if-changed=csrc/guest_shims.c");
    println!("cargo:rerun-if-changed=csrc/stdio_shims.c");
    println!("cargo:rerun-if-changed=build.rs");
    cc::Build::new()
        .file("csrc/guest_shims.c")
        .file("csrc/stdio_shims.c")
        .warnings(true)
        .opt_level(2)
        .compile("acos_native_loader_guest_shims");
}
