fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/emulator_controller.proto");
    tonic_build::configure()
        .build_client(false)
        .compile_protos(&["proto/emulator_controller.proto"], &["proto"])?;
    Ok(())
}
