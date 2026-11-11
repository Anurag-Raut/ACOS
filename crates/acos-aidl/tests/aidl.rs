//! Acceptance tests for `acos-aidl`.
//!
//! These run against the *real* Android 14 boot-classpath DEX files and the
//! measured work-lists. Run with the default workspace root, or point
//! `ACOS_AIDL_WORKSPACE` / `ACOS_AIDL_BOOT` elsewhere.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use acos_aidl::decode::transaction_constants;
use acos_aidl::generate::{
    coverage, read_subset_rows, render_tables, scan_default, subset_methods, BootScanner,
};
use acos_aidl::ir::WireOp;
use acos_aidl::{interface_for_descriptor, lookup_by_name, methods};

fn workspace_root() -> PathBuf {
    if let Ok(p) = std::env::var("ACOS_AIDL_WORKSPACE") {
        return PathBuf::from(p);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or(manifest)
}

fn boot_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ACOS_AIDL_BOOT") {
        return PathBuf::from(p);
    }
    workspace_root().join(".cache/boot/android-34")
}

fn results_dir() -> PathBuf {
    let d = workspace_root().join("results/aidl");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn load_boot() -> BootScanner {
    BootScanner::load(boot_dir()).expect("boot classpath loads")
}

/// (a) Spot-check transaction codes against the real DEX.
#[test]
fn spot_check_codes_against_dex() {
    let scanner = load_boot();
    let interfaces = [
        "android.view.IWindowSession",
        "android.app.IActivityTaskManager",
        "android.hardware.display.IDisplayManager",
        "android.content.IContentProvider",
        "android.view.IWindow",
    ];

    for iface in interfaces {
        let info = scanner
            .decode_interface(iface)
            .unwrap_or_else(|| panic!("{iface} decodes"));
        assert!(
            info.methods.len() >= 8,
            "{iface}: only {} methods decoded",
            info.methods.len()
        );

        // Cross-check every `TRANSACTION_<name>` static constant against the
        // decoded `onTransact` dispatch. This is the mechanical name<->code
        // assertion, over the whole interface.
        let (di, stub_desc) = scanner.find_stub(iface).unwrap();
        let dex = &scanner.dexes[di].dex;
        let def = dex.find_class(&stub_desc).unwrap();
        let consts = transaction_constants(dex, def);
        for (name, code) in &consts {
            let m = info
                .method_by_code(*code as u32)
                .unwrap_or_else(|| panic!("{iface}: code {code} (TRANSACTION_{name}) missing"));
            assert_eq!(&m.name, name, "{iface}: code {code} mismatch");
        }
    }

    // Hand-checked entries read from `dexdump -d` of the real DEX.
    let atms = scanner.decode_interface("android.view.IWindowSession").unwrap();
    assert_eq!(atms.method_by_name("relayout").unwrap().code, 5);
    let on_back = atms.method_by_name("setOnBackInvokedCallbackInfo").unwrap();
    assert_eq!(on_back.code, 39);
    // ...whose case block reads `OnBackInvokedCallbackInfo.CREATOR`.
    assert!(
        on_back
            .args
            .iter()
            .any(|o| matches!(o, WireOp::TypedObject { class, .. } if class == "android.window.OnBackInvokedCallbackInfo")),
        "setOnBackInvokedCallbackInfo should read OnBackInvokedCallbackInfo: {:?}",
        on_back.args
    );
    assert!(on_back.oneway, "setOnBackInvokedCallbackInfo is oneway");

    let dm = scanner
        .decode_interface("android.hardware.display.IDisplayManager")
        .unwrap();
    assert_eq!(dm.method_by_name("getDisplayInfo").unwrap().code, 1);

    let cp = scanner
        .decode_interface("android.content.IContentProvider")
        .unwrap();
    assert_eq!(cp.method_by_name("call").unwrap().code, 21);
}

/// (c) Layout spot-checks, including the relayout byte-op dump.
#[test]
fn layout_spot_checks() {
    let scanner = load_boot();

    let dm = scanner
        .decode_interface("android.hardware.display.IDisplayManager")
        .unwrap();
    let gdi = dm.method_by_name("getDisplayInfo").unwrap();
    assert_eq!(gdi.args, vec![WireOp::I32], "getDisplayInfo args");
    assert_eq!(
        gdi.reply,
        vec![WireOp::TypedObject {
            class: "android.view.DisplayInfo".into(),
            nullable: true,
        }],
        "getDisplayInfo reply"
    );

    let ws = scanner.decode_interface("android.view.IWindowSession").unwrap();
    let relayout = ws.method_by_name("relayout").unwrap();
    let expected_args = vec![
        WireOp::Binder {
            iface: Some("android.view.IWindow".into()),
        },
        WireOp::TypedObject {
            class: "android.view.WindowManager$LayoutParams".into(),
            nullable: true,
        },
        WireOp::I32,
        WireOp::I32,
        WireOp::I32,
        WireOp::I32,
        WireOp::I32,
        WireOp::I32,
    ];
    assert_eq!(relayout.args, expected_args, "relayout args");

    // Write the full decoded relayout sequence for eyeballing.
    let mut out = String::new();
    out.push_str("# IWindowSession.relayout wire layout (mechanically decoded)\n\n");
    out.push_str(&format!("code: {}\n", relayout.code));
    out.push_str(&format!("signature: {}\n", relayout.signature));
    out.push_str(&format!("oneway: {}\n\n", relayout.oneway));
    out.push_str("args (request Parcel, interface token excluded):\n");
    for (i, op) in relayout.args.iter().enumerate() {
        out.push_str(&format!("  {i:>2}: {}\n", op.render()));
    }
    out.push_str("\nreply (reply Parcel, readException excluded):\n");
    for (i, op) in relayout.reply.iter().enumerate() {
        out.push_str(&format!("  {i:>2}: {}\n", op.render()));
    }
    out.push('\n');
    std::fs::write(results_dir().join("relayout.txt"), out).unwrap();

    // The big relayed argument list must match the proxy bytecode op-by-op.
    assert_eq!(
        relayout.args,
        expected_args,
        "relayout proxy byte-op mismatch"
    );

    // A nullable `CharSequence` argument is `writeInt(present)` + a
    // `TextUtils.writeToParcel` payload (`writeInt(notSpanned)` + string8), so
    // the decoded layout must be a doubly-conditional `Str8` — not the two bare
    // `I32`s the decoder emitted before. Getting this wrong made the parcel
    // reader misalign and every toast request fail with
    // `arg 7 … is not a binder`.
    let nm = scanner
        .decode_interface("android.app.INotificationManager")
        .unwrap();
    let toast = nm.method_by_name("enqueueTextToast").unwrap();
    let char_sequence = WireOp::Cond {
        marker: Box::new(WireOp::I32),
        ops: vec![WireOp::Cond {
            marker: Box::new(WireOp::I32),
            ops: vec![WireOp::Str8],
        }],
    };
    assert_eq!(
        toast.args,
        vec![
            WireOp::Str16,
            WireOp::Binder { iface: None },
            char_sequence,
            WireOp::I32,
            WireOp::Bool,
            WireOp::I32,
            WireOp::Binder {
                iface: Some("android.app.ITransientNotificationCallback".into())
            },
        ],
        "enqueueTextToast args"
    );
}

/// (b) Every measured work-list row must resolve in the generated table.
#[test]
fn measured_subset_rows_resolve() {
    let rows = read_subset_rows(&workspace_root()).expect("work-lists read");
    let mut total = 0usize;
    let mut on_transact = 0usize;
    let mut misses: Vec<String> = Vec::new();

    for row in &rows {
        total += 1;
        if row.is_on_transact() {
            on_transact += 1;
            if interface_for_descriptor(&row.interface).is_none() {
                misses.push(format!(
                    "{}: {} (onTransact; interface absent)",
                    row.interface, row.source
                ));
            }
            continue;
        }
        let name = row.lookup_name();
        if name.is_empty() {
            continue;
        }
        if lookup_by_name(&row.interface, name).is_none() {
            misses.push(format!(
                "{}::{} (kind={}, source={})",
                row.interface, name, row.kind, row.source
            ));
        }
    }

    // Report every miss with its reason, then assert the only misses are the
    // interfaces genuinely absent from the boot-classpath cache (the
    // connectivity APEX is not part of `BOOTCLASSPATH`).
    let mut report = String::new();
    report.push_str("# measured work-list resolution\n\n");
    report.push_str(&format!("rows: {total}\n"));
    report.push_str(&format!("onTransact pseudo-rows: {on_transact}\n"));
    report.push_str(&format!("misses: {}\n", misses.len()));
    for miss in &misses {
        report.push_str(&format!("  {miss}\n"));
        eprintln!("subset miss: {miss}");
    }
    std::fs::write(results_dir().join("subset_misses.txt"), report).unwrap();
    // Two legitimate misses in this boot-classpath snapshot:
    //  * `android.net.IConnectivityManager` lives in a connectivity APEX that is
    //    not part of the captured `BOOTCLASSPATH`;
    //  * `IWindow.windowFocusChanged` was removed from the API 34 `IWindow`
    //    AIDL (no `TRANSACTION_windowFocusChanged` in the DEX at all).
    let unexpected: Vec<&String> = misses
        .iter()
        .filter(|m| {
            !m.starts_with("android.net.IConnectivityManager")
                && !m.starts_with("android.view.IWindow::windowFocusChanged")
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "unexpected subset misses: {unexpected:#?}"
    );
    eprintln!(
        "subset: rows={total} onTransact={on_transact} misses={} (all missing interface)",
        misses.len()
    );
}

/// (d) Coverage report for the default subset.
#[test]
fn coverage_report() {
    let scanner = load_boot();
    let rows = read_subset_rows(&workspace_root()).unwrap();
    let (ifaces, missing) = scan_default(&scanner, &rows);
    let subset = subset_methods(&rows);
    let cov = coverage(&ifaces, Some(&subset));

    let mut out = String::new();
    out.push_str("# acos-aidl default-subset coverage\n\n");
    out.push_str(&format!("interfaces: {}\n", cov.interfaces));
    out.push_str(&format!("methods: {}\n", cov.methods));
    out.push_str(&format!(
        "fully decoded: {} ({:.1}%)\n",
        cov.fully_decoded,
        cov.percent()
    ));
    out.push_str(&format!("interfaces absent from boot classpath: {}\n", missing.len()));
    for (iface, reason) in &missing {
        out.push_str(&format!("  missing {iface}: {reason}\n"));
    }
    out.push_str("\nUnknowns (hand-written Rust codecs needed):\n");
    let mut by_reason: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for u in &cov.unknown {
        by_reason
            .entry(u.reason.clone())
            .or_default()
            .push(format!("{}.{} (code {})", u.interface, u.method, u.code));
    }
    if by_reason.is_empty() {
        out.push_str("  (none)\n");
    }
    for (reason, list) in &by_reason {
        out.push_str(&format!("  cause: {reason}\n"));
        for l in list {
            out.push_str(&format!("    {l}\n"));
        }
    }
    std::fs::write(results_dir().join("coverage.txt"), out).unwrap();

    assert!(
        cov.percent() >= 90.0,
        "coverage {:.1}% below target: {:?}",
        cov.percent(),
        cov.unknown
    );
}

/// (e) Regeneration is byte-identical.
#[test]
fn tables_are_deterministic() {
    let scanner = load_boot();
    let rows = read_subset_rows(&workspace_root()).unwrap();
    let (ifaces, _) = scan_default(&scanner, &rows);
    let regenerated = render_tables(&ifaces);
    let committed = include_str!("../src/generated/tables.rs");
    assert_eq!(
        regenerated, committed,
        "generated tables.rs is stale or non-deterministic; rerun the gen example"
    );

    // Second render must be identical too.
    let again = render_tables(&ifaces);
    assert_eq!(regenerated, again, "render is not deterministic");
}

/// (f) Generator benchmark (default subset and full boot classpath).
#[test]
fn generator_benchmark() {
    let scanner = load_boot();
    let rows = read_subset_rows(&workspace_root()).unwrap();

    let t0 = Instant::now();
    let (ifaces, _) = scan_default(&scanner, &rows);
    let default_scan = t0.elapsed();

    let t1 = Instant::now();
    let text = render_tables(&ifaces);
    let default_render = t1.elapsed();

    let t2 = Instant::now();
    let (all, errors) = scanner.decode_all();
    let all_scan = t2.elapsed();

    let t3 = Instant::now();
    let json = acos_aidl::generate::render_json(&all).unwrap();
    let all_render = t3.elapsed();

    let report = format!(
        "default_scan_ms={:.1}\ndefault_render_ms={:.1}\ndefault_bytes={}\nall_scan_ms={:.1}\nall_render_ms={:.1}\nall_interfaces={}\nall_methods={}\nall_bytes={}\ndecode_errors={}\n",
        default_scan.as_secs_f64() * 1000.0,
        default_render.as_secs_f64() * 1000.0,
        text.len(),
        all_scan.as_secs_f64() * 1000.0,
        all_render.as_secs_f64() * 1000.0,
        all.len(),
        all.iter().map(|i| i.methods.len()).sum::<usize>(),
        json.len(),
        errors.len(),
    );
    std::fs::write(results_dir().join("bench.txt"), &report).unwrap();
    eprintln!("{report}");

    // Generous upper bounds; the acceptance run reports release numbers.
    assert!(all.len() > 500, "expected many boot-classpath interfaces");
    assert!(default_render.as_secs() < 30);
}

/// Iteration API sanity.
#[test]
fn library_api_iterates() {
    let count = methods().count();
    assert!(count > 0);
    for (iface, m) in methods() {
        assert!(iface.method_by_code(m.code).is_some());
        assert_eq!(iface.method_by_name(&m.name).map(|x| x.code), Some(m.code));
    }
    assert!(interface_for_descriptor("not.an.Interface").is_none());
}
