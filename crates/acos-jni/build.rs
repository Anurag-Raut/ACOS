//! Compile the C variadic trampolines required by the JNI ABI.
//!
//! Stable Rust cannot define C-variadic functions, so the `Call*Method`,
//! `NewObject` variadic slots are small C shims that read the promoted arguments
//! using the descriptor supplied by the Rust side and forward them as words.

fn main() {
    println!("cargo:rerun-if-changed=csrc/jni_varargs.c");
    println!("cargo:rerun-if-changed=build.rs");
    cc::Build::new()
        .file("csrc/jni_varargs.c")
        .flag_if_supported("-std=c11")
        .flag_if_supported("-Wall")
        .compile("acos_jni_varargs");
}
