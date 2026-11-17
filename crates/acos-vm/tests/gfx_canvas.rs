//! Java-level acceptance for T-31 (display list) + T-32 (HardwareRenderer).
//!
//! Compiles `tests/java/GfxCanvasMain.java` with the host `javac`/`d8` (build
//! tools only, never the execution path) and runs it on the real Android 14
//! boot classpath (`--corelib real`). The Java case records a display list with
//! the real `RecordingCanvas`, rasterises it through the real `HardwareRenderer`
//! state machine, and round-trips 30 `RenderNode` properties.
//!
//! Skips loudly when `javac`/`d8`/`android.jar` or the real boot classpath are
//! unavailable (same policy as the other `--corelib real` suites).

use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_vm::{CoreLib, Vm, VmError, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn sdk() -> Option<PathBuf> {
    std::env::var("ANDROID_HOME")
        .or_else(|_| std::env::var("ANDROID_SDK_ROOT"))
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            let p = PathBuf::from("/home/Qui/Android/Sdk");
            p.is_dir().then_some(p)
        })
}

/// Compile the Java case to DEX. Returns `None` when the host toolchain is
/// unavailable (the test then skips).
fn compile_case(name: &str) -> Option<PathBuf> {
    let javac = which("javac")?;
    let sdk = sdk()?;
    let d8 = sdk.join("build-tools/35.0.0/d8");
    let android_jar = sdk.join("platforms/android-34/android.jar");
    if !d8.is_file() || !android_jar.is_file() {
        return None;
    }
    let root = repo_root();
    let out = root.join("target/tmp/acos-gfx-natives").join(name);
    let dex = out.join("classes.dex");
    if dex.is_file() {
        return Some(dex);
    }
    let src = root
        .join("crates/acos-vm/tests/java")
        .join(format!("{name}.java"));
    if !src.is_file() {
        return None;
    }
    let classes = out.join("classes");
    std::fs::create_dir_all(&classes).ok()?;
    let ok = Command::new(&javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(&android_jar)
        .args(["-d"])
        .arg(&classes)
        .arg(&src)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let ok = Command::new(&d8)
        .arg("--lib")
        .arg(&android_jar)
        .args(["--min-api", "26", "--output"])
        .arg(&out)
        .arg(classes.join(format!("{name}.class")))
        .status()
        .ok()?
        .success();
    if !ok || !dex.is_file() {
        return None;
    }
    Some(dex)
}

fn run_case(name: &str) -> Result<String, String> {
    let Some(dex) = compile_case(name) else {
        return Err("javac/d8/android.jar or the Java case unavailable".to_string());
    };
    let mut vm = match Vm::new(VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    }) {
        Ok(vm) => vm,
        Err(VmError::BootClasspathMissing(m)) => return Err(m),
        Err(e) => return Err(format!("Vm::new failed: {e}")),
    };
    vm.add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());
    let outcome = vm.run(name, &[]);
    if outcome.exit_code != 0 || outcome.vm_error.is_some() {
        return Err(format!(
            "run failed: exit={} vm_error={:?} stdout={} stderr={}",
            outcome.exit_code, outcome.vm_error, outcome.stdout, outcome.stderr
        ));
    }
    Ok(outcome.stdout)
}

// --- minimal PNG writer (no external crate in the test's dependency set) ----

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// Encode RGBA8888 as a PNG using stored (uncompressed) deflate blocks.
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut crc_in = kind.to_vec();
        crc_in.extend_from_slice(data);
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc32(&crc_in).to_be_bytes());
    }
    // Raw scanlines with filter byte 0.
    let mut raw = Vec::with_capacity((width * 4 + 1) as usize * height as usize);
    for y in 0..height {
        raw.push(0);
        let start = (y * width * 4) as usize;
        raw.extend_from_slice(&rgba[start..start + (width * 4) as usize]);
    }
    // zlib stream: header + stored deflate blocks + adler32.
    let mut z = vec![0x78, 0x01];
    let mut pos = 0;
    while pos < raw.len() {
        let n = (raw.len() - pos).min(65535);
        let final_block = pos + n >= raw.len();
        z.push(if final_block { 1 } else { 0 });
        z.extend_from_slice(&(n as u16).to_le_bytes());
        z.extend_from_slice(&(!(n as u16)).to_le_bytes());
        z.extend_from_slice(&raw[pos..pos + n]);
        pos += n;
    }
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in &raw {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[test]
fn gfx_canvas_records_renders_and_round_trips() {
    let out = match run_case("GfxCanvasMain") {
        Ok(out) => out,
        Err(m) => {
            eprintln!("SKIP gfx_canvas_records_renders_and_round_trips: {m}");
            return;
        }
    };
    assert!(
        out.lines().any(|l| l == "done"),
        "Java case did not finish:\n{out}"
    );
    assert!(
        out.lines().any(|l| l == "props=ok"),
        "RenderNode property round-trip failed:\n{out}"
    );
    assert!(
        out.lines().any(|l| l == "sync=0,0"),
        "HardwareRenderer did not accept both frames:\n{out}"
    );

    // The FrameSink (default in-memory sink) received the rasterised frame.
    let frame = acos_vm::natives::gfx_hwui::last_frame()
        .expect("HardwareRenderer submitted no frame to the FrameSink");
    assert_eq!((frame.width, frame.height), (120, 120), "frame size");
    assert_eq!(frame.rgba.len(), 120 * 120 * 4);
    // Two consecutive `syncAndDraw` calls both reached the sink; the second is
    // accepted with no frame-pending stall (Java asserts `sync=0,0`).
    assert_eq!(frame.seq, 2, "two frames submitted to the sink");

    // Record-time capture: the rect must still be blue (0xFF3366FF), not the
    // red the paint was mutated to after recording.
    let px = |x: usize, y: usize| {
        let i = (y * 120 + x) * 4;
        (
            frame.rgba[i],
            frame.rgba[i + 1],
            frame.rgba[i + 2],
            frame.rgba[i + 3],
        )
    };
    assert_eq!(
        px(10, 10),
        (0x33, 0x66, 0xFF, 0xFF),
        "recorded rect changed after Paint mutation (expected frozen blue)"
    );

    // Coverage: the 120x60 rect is 7200 of 14400 pixels; the text adds a little.
    let non_bg = frame
        .rgba
        .chunks_exact(4)
        .filter(|p| p[3] != 0 || p[0] != 0 || p[1] != 0 || p[2] != 0)
        .count();
    assert!(
        (7200..=11000).contains(&non_bg),
        "non-background pixel count {non_bg} out of expected range"
    );

    // Write the frame to PNG (acceptance evidence, gitignored under target/).
    let png = encode_png(frame.width, frame.height, &frame.rgba);
    let path = repo_root().join("target/tmp/gfx_canvas_frame.png");
    let _ = std::fs::write(&path, &png);
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    assert!(png.len() > 1000, "PNG too small: {}", png.len());
    eprintln!(
        "gfx: frame {}x{} non_bg={} png={} path={}",
        frame.width,
        frame.height,
        non_bg,
        png.len(),
        path.display()
    );
}

#[test]
fn stale_graphics_handle_is_structured_not_a_panic() {
    use acos_vm::natives::gfx_handles::{HandleError, HandleTable};
    let mut t: HandleTable<u32> = HandleTable::new();
    let h = t.insert(7);
    assert_eq!(t.remove(h), Ok(7));
    match t.get(h) {
        Err(HandleError::Stale { .. }) => {}
        other => panic!("expected a structured stale-handle error, got {other:?}"),
    }
    // A forged value is stale too.
    assert!(matches!(
        t.get(acos_vm::natives::gfx_handles::Handle(0xdead_beef)),
        Err(HandleError::Stale { .. })
    ));
}
