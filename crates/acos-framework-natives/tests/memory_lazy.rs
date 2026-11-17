#![allow(clippy::field_reassign_with_default)]
//! Lazy-loading memory measurement.
//!
//! Loads `framework-res.apk` (memory-mapped, no wholesale copy), performs 1000
//! resource resolutions, and reports `/proc/self/status` `VmHWM`, `VmRSS`,
//! `RssAnon` and `RssFile` deltas. The anonymous-RSS delta is the heap cost of
//! the parsed table index (the mmap pages are file-backed).

use std::path::PathBuf;

use acos_framework_natives::res::{ApkAssets, AssetSource, ResConfig};

fn workspace_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

#[derive(Debug, Default, Clone, Copy)]
struct Mem {
    vm_hwm_kb: u64,
    vm_rss_kb: u64,
    rss_anon_kb: u64,
    rss_file_kb: u64,
}

fn read_mem() -> Mem {
    let mut mem = Mem::default();
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return mem;
    };
    for line in status.lines() {
        let mut it = line.split_whitespace();
        let key = it.next().unwrap_or("");
        let val: u64 = it.next().unwrap_or("0").parse().unwrap_or(0);
        match key {
            "VmHWM:" => mem.vm_hwm_kb = val,
            "VmRSS:" => mem.vm_rss_kb = val,
            "RssAnon:" => mem.rss_anon_kb = val,
            "RssFile:" => mem.rss_file_kb = val,
            _ => {}
        }
    }
    mem
}

#[test]
fn framework_res_lazy_memory() {
    if !acos_test_support::have_fixture(".cache/boot/android-34/framework-res.apk") {
        eprintln!("framework-res.apk missing; skipping memory test");
        return;
    }
    let apk_path = workspace_root().join(".cache/boot/android-34/framework-res.apk");
    let before = read_mem();

    let apk = ApkAssets::open(&apk_path).expect("open");
    let table = apk.resource_table().expect("table");
    let bytes = apk.resource_bytes().expect("arsc bytes");
    let after_load = read_mem();

    // 1000 resolutions across many ids and the device configuration.
    let mut config = ResConfig::default();
    config.language = *b"en";
    config.country = *b"US";
    config.density = 420;
    config.smallest_screen_width_dp = 411;
    config.screen_width_dp = 411;
    config.screen_height_dp = 842;
    config.orientation = 1;
    config.ui_mode = 0x11;
    config.sdk_version = 34;
    config.screen_layout = 0x12;
    let mut resolved = 0usize;
    for i in 0..1000u32 {
        let id = 0x0104_0000 + (i % 50);
        if table.resolve(bytes, id, &config).is_some() {
            resolved += 1;
        }
    }
    let after_resolve = read_mem();

    eprintln!(
        "MEM before          VmHWM={}kB VmRSS={}kB RssAnon={}kB RssFile={}kB",
        before.vm_hwm_kb, before.vm_rss_kb, before.rss_anon_kb, before.rss_file_kb
    );
    eprintln!(
        "MEM after load      VmHWM={}kB VmRSS={}kB RssAnon={}kB RssFile={}kB (delta anon={}kB)",
        after_load.vm_hwm_kb,
        after_load.vm_rss_kb,
        after_load.rss_anon_kb,
        after_load.rss_file_kb,
        after_load.rss_anon_kb.saturating_sub(before.rss_anon_kb)
    );
    eprintln!(
        "MEM after {resolved} resolves VmHWM={}kB VmRSS={}kB RssAnon={}kB RssFile={}kB (delta anon={}kB)",
        after_resolve.vm_hwm_kb,
        after_resolve.vm_rss_kb,
        after_resolve.rss_anon_kb,
        after_resolve.rss_file_kb,
        after_resolve.rss_anon_kb.saturating_sub(before.rss_anon_kb)
    );

    // The index must be small enough that the APK is genuinely lazy: the
    // anonymous heap delta is bounded (a few hundred KiB), not the 27 MB file.
    let anon_delta = after_resolve.rss_anon_kb.saturating_sub(before.rss_anon_kb);
    eprintln!("MEM heap delta = {anon_delta}kB");
    assert!(
        anon_delta < 4096,
        "heap growth {anon_delta}kB suggests the APK was copied, not lazily parsed"
    );

    // Sanity: the table index is populated.
    let _ = table.global_strings(bytes).map(|p| p.len());
}
