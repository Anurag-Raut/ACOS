//! Write the scripted-camera test frames (`test-frame.png`, `test-frame.jpg`) into the
//! directory given as the only argument. `tools/build/build-apps.sh` runs this into
//! `fixtures/build/camera/`; the frames are generated, never committed.
//!
//! cargo run --offline -q -p acos-services --example gen_camera_frames -- fixtures/build/camera

fn main() {
    let dir = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            eprintln!("usage: gen_camera_frames <out-dir>");
            std::process::exit(2);
        });
    if let Err(e) = acos_services::modules::camera::write_test_frames(&dir) {
        eprintln!("gen_camera_frames: {e}");
        std::process::exit(1);
    }
    println!("gen_camera_frames: wrote {}/test-frame.{{png,jpg}}", dir.display());
}
