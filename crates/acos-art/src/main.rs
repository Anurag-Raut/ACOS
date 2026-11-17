//! `acos-art` — run DEX on real ART inside the ACOS mini-Android root.
//!
//! ```text
//! acos-art run   --dex classes.dex [--class Main] [--json]
//! acos-art verify [--fixtures DIR] [--json OUT]      # the 27-program corpus
//! acos-art build-image [--out DIR] [--force]         # AOT boot image (dex2oat64)
//! acos-art build-shim                                # libartshim.so
//! acos-art props [print|write|get KEY]
//! acos-art doctor                                    # layout/capability report
//! acos-art render-app --apk FILE [--out DIR]          # first frame + view tree
//! acos-art launch --apk FILE [--until application-created]  # framework startup
//! acos-art build-app                                  # guest dex + substrate shim
//! ```

use acos_art::{asset_path, launcher, ArtRoot, Error, JitMode, LogMode, Props, Result, RunOpts};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "\
acos-art — real Android 14 ART as an ACOS DEX engine (bwrap mini-Android root)

USAGE:
  acos-art run   --dex FILE [--dex FILE]... [--class NAME] [ARGS...]
  acos-art verify [--programs DIR] [--expected DIR] [--json FILE]
  acos-art build-image [--out DIR] [--force]
  acos-art build-shim
  acos-art props [print|write|get KEY]
  acos-art doctor
  acos-art render-app --apk FILE [--out DIR] [--package PKG] [--layout NAME]
                     [--width PX] [--panel-height PX] [--content-height PX]
                     [--status-inset PX] [--nav-inset PX] [--density DPI]
                     [--text-allcaps device|theme]
  acos-art launch --apk FILE [--until POINT] [--out DIR] [--width PX]
                     [--panel-height PX] [--status-inset PX] [--nav-inset PX]
                     [--density DPI] [--insets carve|report]
                     [--animations on|off]
                     [--extra-apk FILE]...  # install more packages in this
                                            # session (repeatable)
  acos-art session --apk FILE [--out DIR] [--package PKG] [--activity CLASS]
                     [--width PX] [--panel-height PX] [--status-inset PX]
                     [--nav-inset PX] [--density DPI] [--session-id ID]
                     [--socket PATH]
                     # line-JSON on stdin/stdout, or on --socket PATH:
                     # screenshot|tree|tap|key|reset|close
  acos-art build-app
  acos-art janitor                      # reap stale per-run data roots (S4)

RESOURCE LIMITS (ADR-0018 / R1):
  ACOS_ART_RSS_MAX   launcher RSS ceiling (default 2G, 0 = off): crossing it
                     kills the guest process group and reports HOST_MEMORY_LIMIT
  ACOS_ART_RLIMIT_DATA  soft RLIMIT_DATA backstop (default 1.25 x the above, 0 = off)
  ACOS_ART_STREAM_CAP   captured guest stdout/stderr bytes (default 8M)
  ACOS_ART_LOG_CAP      logd entries retained (default 50000, drop-oldest)

COMMON OPTIONS:
  --sysroot DIR      ART sysroot (default: $ACOS_ART_SYSROOT, .cache/art/sysroot,
                     .cache/art-exp/sysroot)
  --data DIR         writable /data (default: $ACOS_ART_DATA, .cache/art/data)
  --timeout SECS     wall-clock budget (default 60)
  --rlimit-as-mb N   RLIMIT_AS in MiB, 0 = unlimited (default 0: Scudo
                     reserves ~8.6 TB of virtual address space)
  --mode MODE        default | int  (-Xint)
  --log MODE         logd | stderr   (default logd)
  --no-shim          do not LD_PRELOAD libartshim.so (no props, no logd sink)
  --no-props         do not write the property table
  --image DIR        boot image dir (contains x86_64/boot.art); `none` disables
  --json             print the structured RunResult instead of a summary
";

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("acos-art: {e}");
            ExitCode::from(2)
        }
    }
}

fn real_main() -> Result<ExitCode> {
    // Defence in depth, once per process: the launcher's own soft RLIMIT_DATA
    // backstop plus the RSS watchdog (ACOS_ART_RSS_MAX, default 2 GiB). Every
    // guest `pre_exec` raises the rlimit again, so ART is never bounded by it.
    let (mut _watchdog, data_limit) = acos_art::watchdog::install_process_guards();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" || args[0] == "help" {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if data_limit > 0 && std::env::var_os("ACOS_ART_QUIET_GUARD").is_none() {
        eprintln!(
            "acos-art: guards rss_max={}MiB rlimit_data={}MiB stream_cap={}MiB log_cap={}",
            acos_art::watchdog::rss_max_from_env() / 1024 / 1024,
            data_limit / 1024 / 1024,
            acos_art::capture::stream_cap_from_env() / 1024 / 1024,
            acos_art::capture::log_cap_from_env(),
        );
    }
    let cmd = args[0].clone();
    // Common options are allowed anywhere; parse them out first.
    let mut rest: Vec<String> = Vec::new();
    let mut sysroot = None;
    let mut data = None;
    let mut timeout = 60u64;
    let mut rlimit_as_mb = 0u64;
    let mut mode = JitMode::Default;
    let mut log = LogMode::Logd;
    let mut shim = true;
    let mut props = true;
    let mut image: Option<PathBuf> = None;
    let mut use_image = true;
    let mut json = false;
    let mut class = "Main".to_string();
    let mut dex: Vec<PathBuf> = Vec::new();
    let mut program_args: Vec<String> = Vec::new();
    let mut force = false;
    let mut out: Option<PathBuf> = None;
    let mut json_out: Option<PathBuf> = None;
    let mut programs: Option<PathBuf> = None;
    let mut expected: Option<PathBuf> = None;
    let mut apk: Option<PathBuf> = None;
    let mut until = "application-created".to_string();
    let mut animations = acos_art::launch::Animations::On;
    let mut extra_apks: Vec<PathBuf> = Vec::new();
    // MPROC5c: the idle kill's timeout. `ACOSLAUNCH_IDLE_KILL_SECS` is the
    // plan/env spelling the zygote path already uses; the flag overrides it.
    let mut idle_kill_secs = std::env::var("ACOSLAUNCH_IDLE_KILL_SECS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(30u64);
    let mut package: Option<String> = None;
    let mut layout: Option<String> = None;
    let mut size: (u32, u32, u32) = (1080, 2062, 420);
    let mut panel_height = 2400u32;
    let mut status_inset = 128u32;
    let mut nav_inset = 63u32;
    let mut insets = std::env::var("ACOS_WM_INSETS")
        .unwrap_or_else(|_| acos_services::wms::DEFAULT_INSETS_MODE.as_str().to_string());
    let mut all_caps = acos_art::app::AllCaps::Device;
    let mut activity: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut socket: Option<PathBuf> = None;
    let mut services: Vec<serde_json::Value> = Vec::new();

    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        let mut next = |i: &mut usize| -> Result<String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| Error::Layout(format!("option {a} needs a value")))
        };
        match a {
            "--sysroot" => sysroot = Some(PathBuf::from(next(&mut i)?)),
            "--data" => data = Some(PathBuf::from(next(&mut i)?)),
            "--timeout" => timeout = next(&mut i)?.parse().unwrap_or(60),
            "--rlimit-as-mb" => rlimit_as_mb = next(&mut i)?.parse().unwrap_or(8192),
            "--mode" => {
                mode = match next(&mut i)?.as_str() {
                    "int" | "xint" => JitMode::Int,
                    _ => JitMode::Default,
                }
            }
            "--log" => {
                log = match next(&mut i)?.as_str() {
                    "stderr" => LogMode::Stderr,
                    _ => LogMode::Logd,
                }
            }
            "--no-shim" => shim = false,
            "--no-props" => props = false,
            "--image" => {
                let v = next(&mut i)?;
                if v == "none" {
                    use_image = false;
                    image = None;
                } else {
                    use_image = true;
                    image = Some(PathBuf::from(v));
                }
            }
            "--json" => json = true,
            "--json-out" => json_out = Some(PathBuf::from(next(&mut i)?)),
            "--class" => class = next(&mut i)?,
            "--dex" => dex.push(PathBuf::from(next(&mut i)?)),
            "--programs" | "--fixtures" => programs = Some(PathBuf::from(next(&mut i)?)),
            "--expected" => expected = Some(PathBuf::from(next(&mut i)?)),
            "--out" => out = Some(PathBuf::from(next(&mut i)?)),
            "--apk" => apk = Some(PathBuf::from(next(&mut i)?)),
            "--until" => until = next(&mut i)?,
            "--animations" => {
                let value = next(&mut i)?;
                animations = acos_art::launch::Animations::parse(&value).map_err(Error::Layout)?;
            }

            // ADR-0029 item 4: `--service id[:profile][,config]`, repeatable.
            "--service" => {
                let spec = acos_services::module::ModuleSpec::parse(&next(&mut i)?)
                    .map_err(|error| Error::Layout(error.to_string()))?;
                services.push(spec.to_json());
            }

            // MPROC1: repeatable; each APK is installed into the session beside
            // the launched one, at its own uid, with its own guest paths.
            "--extra-apk" => extra_apks.push(PathBuf::from(next(&mut i)?)),
            // MPROC5c: the idle kill's timeout (0 disables it; tests set small).
            "--idle-kill-seconds" => idle_kill_secs = next(&mut i)?.parse().unwrap_or(30),
            "--package" => package = Some(next(&mut i)?),
            "--activity" => activity = Some(next(&mut i)?),
            "--session-id" => session_id = Some(next(&mut i)?),
            "--socket" => socket = Some(PathBuf::from(next(&mut i)?)),
            "--layout" => layout = Some(next(&mut i)?),
            "--width" => size.0 = next(&mut i)?.parse().unwrap_or(1080),
            "--panel-height" => panel_height = next(&mut i)?.parse().unwrap_or(2400),
            "--content-height" => size.1 = next(&mut i)?.parse().unwrap_or(2062),
            "--status-inset" => status_inset = next(&mut i)?.parse().unwrap_or(128),
            "--nav-inset" => nav_inset = next(&mut i)?.parse().unwrap_or(63),
            "--insets" => {
                let value = next(&mut i)?;
                if acos_services::wms::parse_insets_mode(&value).is_none() {
                    return Err(Error::Layout(format!(
                        "--insets must be carve or report (got {value:?})"
                    )));
                }
                insets = value;
            }
            "--density" => size.2 = next(&mut i)?.parse().unwrap_or(420),
            "--text-allcaps" => {
                all_caps = match next(&mut i)?.as_str() {
                    "theme" => acos_art::app::AllCaps::Theme,
                    _ => acos_art::app::AllCaps::Device,
                }
            }
            "--force" => force = true,
            "--" => {
                program_args.extend(args[i + 1..].iter().cloned());
                break;
            }
            other if other.starts_with('-') => {
                return Err(Error::Layout(format!("unknown option {other}")));
            }
            other => program_args.push(other.to_string()),
        }
        i += 1;
    }

    let root = ArtRoot::resolve(sysroot, data)?;
    match cmd.as_str() {
        "run" => {
            if dex.is_empty() {
                return Err(Error::Layout("run needs --dex FILE".into()));
            }
            let opts = RunOpts {
                dex,
                class,
                class_args: program_args,
                jit: mode,
                timeout: Duration::from_secs(timeout),
                rlimit_as_mb,
                log,
                image,
                use_image,
                shim,
                props,
                extra_env: Vec::new(),
            };
            let res = launcher::run(&root, &opts)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&res).unwrap());
            } else {
                print!("{}", res.stdout);
                eprint!("{}", res.stderr);
                eprintln!(
                    "acos-art: outcome={} code={} exit={:?} signal={:?} wall={}ms",
                    format!("{:?}", res.outcome).to_lowercase(),
                    res.code.as_deref().unwrap_or("-"),
                    res.exit,
                    res.signal,
                    res.wall_ms
                );
                for d in &res.diagnostics {
                    eprintln!("acos-art: {d}");
                }
            }
            Ok(if res.succeeded() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        "verify" => verify(
            &root,
            programs,
            expected,
            json_out,
            timeout,
            rlimit_as_mb,
            json,
        ),
        "build-image" => {
            let dir = build_image(&root, out, force)?;
            println!("{}", dir.display());
            Ok(ExitCode::SUCCESS)
        }
        "build-shim" => {
            let p = acos_art::build_shim(&root.data.join("shim"))?;
            println!("{}", p.display());
            Ok(ExitCode::SUCCESS)
        }
        "build-app" => {
            let guest = acos_art::app::build_guest(&root)?;
            let shim = acos_art::app::build_render_shim(&root)?;
            println!("{}", guest.dex_dir.join("classes.dex").display());
            println!("{}", shim.display());
            Ok(ExitCode::SUCCESS)
        }
        "render-app" => {
            let apk = apk.ok_or_else(|| Error::Layout("render-app needs --apk FILE".into()))?;
            let opts = acos_art::app::AppOpts {
                apk,
                package,
                layout,
                out,
                width: size.0,
                panel_height,
                content_height: size.1,
                status_inset,
                nav_inset,
                density_dpi: size.2,
                text_all_caps: all_caps,
                timeout: Duration::from_secs(timeout),
            };
            let res = acos_art::app::render_app(&root, &opts)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&res).unwrap());
            } else {
                println!(
                    "frame={} tree={} md5={} nodes={} {}x{}@{}",
                    res.frame.as_deref().unwrap_or("-"),
                    res.tree.as_deref().unwrap_or("-"),
                    res.frame_md5.as_deref().unwrap_or("-"),
                    res.nodes
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "-".into()),
                    res.width,
                    res.panel_height,
                    res.density_dpi
                );
                println!(
                    "outcome={} code={} wall={}ms draw_cold={}us draw_warm={}us peak_rss={}MB",
                    format!("{:?}", res.outcome).to_lowercase(),
                    res.code.as_deref().unwrap_or("-"),
                    res.wall_ms,
                    res.draw_cold_us
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".into()),
                    res.draw_warm_us
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".into()),
                    res.peak_rss_mb
                        .map(|v| format!("{v:.0}"))
                        .unwrap_or_else(|| "-".into())
                );
                for d in &res.diagnostics {
                    eprintln!("acos-art: {d}");
                }
            }
            Ok(if res.succeeded() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        "session" => {
            let apk = apk.ok_or_else(|| Error::Layout("session needs --apk FILE".into()))?;
            let opts = acos_art::session::SessionOpts {
                apk,
                package,
                activity,
                out,
                width: size.0,
                panel_height,
                content_height: size.1,
                status_inset,
                nav_inset,
                density_dpi: size.2,
                timeout: Duration::from_secs(timeout),
                session_id,
                socket,
            };
            let code = acos_art::session::session_cli(&root, &opts)?;
            Ok(ExitCode::from(code as u8))
        }
        "launch" => {
            let apk = apk.ok_or_else(|| Error::Layout("launch needs --apk FILE".into()))?;
            let opts = acos_art::launch::LaunchOpts {
                apk,
                until,
                out,
                timeout: Duration::from_secs(timeout),
                width: size.0,
                panel_height,
                density_dpi: size.2,
                status_inset,
                nav_inset,
                insets,
                animations,

                services,

                extra_apks,
                idle_kill_secs,
            };
            let res = acos_art::launch::launch(&root, &opts)?;
            println!("{}", serde_json::to_string_pretty(&res).unwrap());
            Ok(if res.outcome == acos_art::Outcome::Ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        "props" => {
            let what = program_args.first().map(String::as_str).unwrap_or("print");
            match what {
                "print" => print!("{}", Props::for_launch()?.to_string()),
                "write" => {
                    let p = root.data.join(acos_art::props::PROPS_FILE);
                    Props::for_launch()?.write(&p)?;
                    println!("{}", p.display());
                }
                "get" => {
                    let key = program_args.get(1).cloned().unwrap_or_default();
                    let p = root.data.join(acos_art::props::PROPS_FILE);
                    let props = Props::from_file(&p).unwrap_or_else(|_| Props::android34());
                    println!("{}", props.get(&key).unwrap_or(""));
                }
                other => return Err(Error::Layout(format!("props: unknown action {other}"))),
            }
            Ok(ExitCode::SUCCESS)
        }
        "doctor" => {
            doctor(&root);
            Ok(ExitCode::SUCCESS)
        }
        "janitor" => {
            // Same sweep every run performs at startup, callable on its own so a
            // host can reap an orphaned run root without starting a guest.
            let n = root.sweep_stale_runs()?;
            println!(
                "janitor: removed {n} stale run dir(s) under {}",
                root.runs_dir().display()
            );
            Ok(ExitCode::SUCCESS)
        }
        other => Err(Error::Layout(format!("unknown command {other}"))),
    }
}

// ---------------------------------------------------------------------------
// verify: the 27-program correctness corpus against fixtures/expected-art
// ---------------------------------------------------------------------------

fn verify(
    root: &ArtRoot,
    programs: Option<PathBuf>,
    expected: Option<PathBuf>,
    json_out: Option<PathBuf>,
    timeout: u64,
    rlimit_as_mb: u64,
    json: bool,
) -> Result<ExitCode> {
    let programs = programs.unwrap_or_else(|| crate::asset_path("fixtures/build/dexprogs"));
    let expected = expected.unwrap_or_else(|| crate::asset_path("fixtures/expected-art/dexprogs"));
    let mut names: Vec<String> = std::fs::read_dir(&expected)?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            if p.extension()?.to_str()? == "exit" {
                Some(p.file_stem()?.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    if names.is_empty() {
        return Err(Error::Layout(format!(
            "no .exit fixtures in {}",
            expected.display()
        )));
    }

    println!(
        "{:<30} {:<5} {:>6} {:>5} {:>7} {}",
        "program", "pass", "exit", "exp", "wall_ms", "notes"
    );
    let mut rows = Vec::new();
    let mut pass = 0usize;
    let mut skipped = 0usize;
    let mut fatal_total = 0usize;
    for name in &names {
        let dex = programs.join(name).join("classes.dex");
        if !dex.is_file() {
            println!(
                "{name:<30} {:<5} {:>6} {:>5} {:>7} {}",
                "skip", "-", "-", "-", "no dex built"
            );
            rows.push(serde_json::json!({"program": name, "skip": true, "note": "no dex built"}));
            skipped += 1;
            continue;
        }
        let exp_out =
            std::fs::read_to_string(expected.join(format!("{name}.stdout"))).unwrap_or_default();
        let exp_exit: i32 = std::fs::read_to_string(expected.join(format!("{name}.exit")))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let opts = RunOpts {
            dex: vec![dex],
            class: "Main".into(),
            class_args: Vec::new(),
            jit: JitMode::Default,
            timeout: Duration::from_secs(timeout),
            rlimit_as_mb,
            log: LogMode::Logd,
            image: None,
            use_image: true,
            shim: true,
            props: true,
            extra_env: Vec::new(),
        };
        let res = launcher::run(root, &opts)?;
        let stdout_ok = res.stdout == exp_out;
        let exit_ok = res.exit == Some(exp_exit) && res.outcome == acos_art::Outcome::Ok;
        let ok = stdout_ok && exit_ok;
        if ok {
            pass += 1;
        }
        let mut notes = Vec::new();
        if !stdout_ok {
            notes.push("stdout differs".to_string());
        }
        if !exit_ok {
            notes.push(format!(
                "outcome={:?} code={}",
                res.outcome,
                res.code.as_deref().unwrap_or("-")
            ));
        }
        // Soft check: every fixture stderr line should appear in the real stderr.
        let exp_err =
            std::fs::read_to_string(expected.join(format!("{name}.stderr"))).unwrap_or_default();
        let stderr_ok = exp_err
            .lines()
            .filter(|l| !l.trim().is_empty())
            .all(|l| res.stderr.contains(l));
        if !stderr_ok && !exp_err.trim().is_empty() {
            notes.push("stderr differs".into());
        }
        println!(
            "{:<30} {:<5} {:>6} {:>5} {:>7} {}",
            name,
            if ok { "PASS" } else { "FAIL" },
            res.exit
                .map(|c| c.to_string())
                .unwrap_or_else(|| "sig".into()),
            exp_exit,
            res.wall_ms,
            notes.join(", ")
        );
        let fatal_logs = res.logs.iter().filter(|e| e.is_fatal()).count();
        if fatal_logs > 0 {
            notes.push(format!("{fatal_logs} fatal ART log entries"));
        }
        fatal_total += fatal_logs;
        rows.push(serde_json::json!({
            "program": name,
            "pass": ok,
            "stdout_ok": stdout_ok,
            "stderr_ok": stderr_ok,
            "exit": res.exit,
            "expected_exit": exp_exit,
            "outcome": res.outcome,
            "code": res.code,
            "wall_ms": res.wall_ms,
            "logs": res.logs.len(),
            "fatal_logs": fatal_logs,
        }));
    }

    let total = names.len() - skipped;
    println!(
        "\n{pass}/{total} pass, {} fail ({} skipped)",
        total - pass,
        skipped
    );
    // ART must reach real fatal-abort behaviour: no suppressed CHECKs anywhere.
    println!("fatal ART log entries recorded: {fatal_total}");
    let summary = serde_json::json!({
        "engine": "art-dalvikvm64",
        "sysroot": root.sysroot.display().to_string(),
        "programs": total,
        "skipped": skipped,
        "pass": pass,
        "fail": total - pass,
        "results": rows,
    });
    if let Some(p) = json_out {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, serde_json::to_string_pretty(&summary).unwrap() + "\n")?;
        eprintln!("wrote {}", p.display());
    } else if json {
        println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    }
    Ok(if pass == total {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

// ---------------------------------------------------------------------------
// build-image: AOT boot image with the image's own dex2oat64
// ---------------------------------------------------------------------------

fn build_image(root: &ArtRoot, out: Option<PathBuf>, force: bool) -> Result<PathBuf> {
    let out = out.unwrap_or_else(|| root.data.join("img"));
    let named = out.join("x86_64");
    std::fs::create_dir_all(&named)?;
    if !force && named.join("boot.art").is_file() && named.join("boot.oat").is_file() {
        eprintln!("acos-art: boot image already built at {}", named.display());
        return Ok(out);
    }
    // H13: the boot-image builder is shared with the `shared-aslr` preparation
    // path. `build-image` keeps the product's stock LZ4 format and fixed base.
    let report =
        acos_art::boot_image::build_to(root, &out, "lz4", acos_art::boot_image::ART_BASE_ADDRESS)?;
    if !named.join("boot.art").is_file() {
        return Err(Error::Layout(format!(
            "dex2oat produced no {}",
            named.join("boot.art").display()
        )));
    }
    std::fs::write(
        out.join("MANIFEST.txt"),
        format!(
            "boot image for {} built {} from {} boot classpath jars\n",
            "android-34 x86_64",
            chrono_like_now(),
            report.jars
        ),
    )?;
    Ok(out)
}

fn chrono_like_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

// ---------------------------------------------------------------------------
// doctor
// ---------------------------------------------------------------------------

fn doctor(root: &ArtRoot) {
    let ok = |b: bool| if b { "ok" } else { "MISSING" };
    println!("repo root      {}", root.repo_root.display());
    println!("sysroot        {}", root.sysroot.display());
    println!("data           {}", root.data.display());
    println!(
        "apex modules   {} ({})",
        root.modules.len(),
        ok(root.modules.len() > 30)
    );
    let bwrap = which("bwrap");
    println!(
        "bwrap          {}",
        bwrap
            .as_deref()
            .map(Path::display)
            .map(|d| d.to_string())
            .unwrap_or("MISSING".into())
    );
    let dalvik = root
        .sysroot
        .join("system/apex/com.android.art/bin/dalvikvm64");
    println!(
        "dalvikvm64     {} {}",
        dalvik.display(),
        if dalvik.is_file() { "" } else { "(MISSING)" }
    );
    let dex2oat = root
        .sysroot
        .join("system/apex/com.android.art/bin/dex2oat64");
    println!(
        "dex2oat64      {} {}",
        dex2oat.display(),
        if dex2oat.is_file() { "" } else { "(MISSING)" }
    );
    match root.device_bootclasspath() {
        Ok(jars) => println!(
            "bootclasspath  {} jars, first={}",
            jars.len(),
            jars.first().cloned().unwrap_or_default()
        ),
        Err(e) => println!("bootclasspath  ERROR: {e}"),
    }
    match root.classpaths() {
        // The device pair ART's mainline-framework lookup needs.
        Some(cp) => println!(
            "classpaths     boot={} dex2oat={} (mainline from {}), via derive_classpath",
            cp.boot.len(),
            cp.dex2oat.len(),
            cp.boot.get(cp.dex2oat.len()).cloned().unwrap_or_else(|| "-".into())
        ),
        None => println!(
            "classpaths     unavailable (no {}: run once, or the sysroot has no /system/etc/classpaths/)",
            root.classpath_env_file().display()
        ),
    }
    let ld = root.data.join("linkerconfig/ld.config.txt");
    println!(
        "linkerconfig   {} {}",
        ld.display(),
        if ld.is_file() {
            ""
        } else {
            "(not generated yet)"
        }
    );
    let img = root.data.join("img/x86_64/boot.art");
    println!(
        "boot image     {} {}",
        img.display(),
        if img.is_file() {
            ""
        } else {
            "(absent: imageless JIT)"
        }
    );
    println!(
        "shim source    {} bytes (compiled on demand with $CC/clang)",
        acos_art::SHIM_SOURCE.len()
    );
    let virt = root.sysroot.join("system/lib64/libartshim.so");
    let _ = virt;
    println!("userns         {}", userns_enabled());
}

fn userns_enabled() -> &'static str {
    match std::fs::read_to_string("/proc/sys/user/max_user_namespaces") {
        Ok(s) if s.trim().parse::<u64>().unwrap_or(0) > 0 => "enabled",
        _ => "DISABLED (bwrap will fail)",
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

#[allow(dead_code)]
fn is_dir(p: &Path) -> bool {
    p.is_dir()
}
