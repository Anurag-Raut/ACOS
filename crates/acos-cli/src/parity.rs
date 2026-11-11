//! `acos parity --device <serial> <apk>` — compare a real phone's UI against
//! ACOS's tree/frame.
//!
//! The live path installs the fixture APK on an explicitly named device, starts
//! it, captures `uiautomator dump` plus a screenshot, captures the ACOS tree
//! from the real `View` hierarchy, and emits a JSON report of matches and
//! differences. It only ever installs/uninstalls an APK whose package starts
//! with `dev.acos.fixtures.`; it never changes device settings.
//!
//! The same comparator runs offline against the recorded captures in
//! `fixtures/reference-device/` (`--recorded <app>`), so the CI test runs with
//! no phone attached. The live path is optional and **skips loudly**: a missing
//! `adb` or device yields a non-zero `ok:false` report with
//! `NEEDS_REAL_DEVICE`, never an empty "pass".
//!
//! Attributes are compared after normalization: `resource_id` (falling back to
//! `class|text`, then `class`), `text` (whitespace-trimmed), `class`, and
//! `bounds` (optional `--bounds-scale` plus `--bounds-tolerance` pixels).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use acos_runtime::view_tree::ViewNode;
use acos_vm::{CoreLib, VmOptions};
use serde::Serialize;
use serde_json::{json, Value};

use crate::observe_tree::{
    flatten, parse_acos_tree_json, parse_agent_device_snapshot, parse_uiautomator_xml, tree_to_json,
};

/// Comparison options.
#[derive(Debug, Clone, Copy)]
pub struct CompareOptions {
    /// Maximum per-edge pixel delta after scaling.
    pub bounds_tolerance: i32,
    /// Factor applied to the *device* bounds before comparing.
    pub bounds_scale: f64,
    /// Ignore bounds entirely (text/class/id parity only).
    pub ignore_bounds: bool,
    /// Compare `enabled`/`focused`/`clickable` too.
    pub strict_state: bool,
    /// Also build the ranked per-element (resource-id) layout diff.
    pub element_diff: bool,
}

impl Default for CompareOptions {
    fn default() -> Self {
        CompareOptions {
            bounds_tolerance: 4,
            bounds_scale: 1.0,
            ignore_bounds: false,
            strict_state: false,
            element_diff: false,
        }
    }
}

/// One element compared **by resource id** between the ACOS tree and the
/// phone/reference tree, with the raw bounds and size deltas.
///
/// This is the geometry-focused companion to [`Difference`]: it only matches
/// nodes that carry a resource id (the stable identity the product path also
/// uses) and reports `acos - phone` per edge plus the per-axis size change, so
/// the largest real layout differences rank first.
#[derive(Debug, Clone, Serialize)]
pub struct ElementDifference {
    /// Android resource id (`pkg:id/name`).
    pub resource_id: String,
    /// View class on the ACOS side (or the phone side when ACOS is missing).
    pub class: String,
    /// ACOS text.
    pub acos_text: String,
    /// Phone text.
    pub phone_text: String,
    /// ACOS bounds `[left, top, right, bottom]`, if present.
    pub acos_bounds: Option<[i32; 4]>,
    /// Phone bounds `[left, top, right, bottom]`, if present.
    pub phone_bounds: Option<[i32; 4]>,
    /// Per-edge `acos - phone` delta, when both bounds exist.
    pub delta: Option<[i32; 4]>,
    /// ACOS size `[width, height]`.
    pub acos_size: Option<[i32; 2]>,
    /// Phone size `[width, height]`.
    pub phone_size: Option<[i32; 2]>,
    /// `acos_size - phone_size`.
    pub size_delta: Option<[i32; 2]>,
    /// `matched`, `bounds_differ`, `text_differs`, `missing_in_acos` or
    /// `missing_on_phone`.
    pub kind: String,
    /// Ranking magnitude: larger means a bigger real difference.
    pub score: i64,
}

/// Bounds deltas smaller than this are likely rounding, not layout drift.
const ELEMENT_DELTA_FLOOR: i64 = 2;
/// Text/size mismatch weight so structural differences outrank tiny shifts.
const ELEMENT_TEXT_WEIGHT: i64 = 100_000;

/// Match two trees by resource id and rank the layout differences.
///
/// Nodes without a resource id are skipped: they have no stable identity to
/// align on, and the product selectors used by `acos tap --id` all carry one.
/// The result is sorted by descending [`ElementDifference::score`].
#[must_use]
pub fn element_differences(acos: &[ViewNode], phone: &[ViewNode]) -> Vec<ElementDifference> {
    use std::collections::BTreeSet;

    fn by_id(nodes: &[ViewNode]) -> BTreeMap<String, &ViewNode> {
        let mut map = BTreeMap::new();
        for node in flatten(nodes) {
            if !node.resource_id.is_empty() {
                map.entry(node.resource_id.clone()).or_insert(node);
            }
        }
        map
    }
    let acos_by_id = by_id(acos);
    let phone_by_id = by_id(phone);
    let keys: BTreeSet<&String> = acos_by_id.keys().chain(phone_by_id.keys()).collect();

    let size = |b: [i32; 4]| [b[2] - b[0], b[3] - b[1]];
    let mut out = Vec::new();
    for key in keys {
        let a = acos_by_id.get(key).copied();
        let p = phone_by_id.get(key).copied();
        let mut entry = ElementDifference {
            resource_id: key.clone(),
            class: a.map(|n| n.class.clone()).or_else(|| p.map(|n| n.class.clone())).unwrap_or_default(),
            acos_text: a.map(|n| n.text.clone()).unwrap_or_default(),
            phone_text: p.map(|n| n.text.clone()).unwrap_or_default(),
            acos_bounds: a.map(|n| n.bounds),
            phone_bounds: p.map(|n| n.bounds),
            delta: None,
            acos_size: a.map(|n| size(n.bounds)),
            phone_size: p.map(|n| size(n.bounds)),
            size_delta: None,
            kind: "matched".to_string(),
            score: 0,
        };
        match (a, p) {
            (None, Some(_)) => {
                entry.kind = "missing_in_acos".to_string();
                entry.score = ELEMENT_TEXT_WEIGHT;
            }
            (Some(_), None) => {
                entry.kind = "missing_on_phone".to_string();
                entry.score = ELEMENT_TEXT_WEIGHT;
            }
            (Some(a), Some(p)) => {
                let delta = [
                    a.bounds[0] - p.bounds[0],
                    a.bounds[1] - p.bounds[1],
                    a.bounds[2] - p.bounds[2],
                    a.bounds[3] - p.bounds[3],
                ];
                entry.delta = Some(delta);
                entry.size_delta = Some([
                    entry.acos_size.unwrap()[0] - entry.phone_size.unwrap()[0],
                    entry.acos_size.unwrap()[1] - entry.phone_size.unwrap()[1],
                ]);
                let mut score: i64 = delta.iter().map(|d| i64::from(d.abs())).sum();
                if a.text.trim() != p.text.trim() {
                    entry.kind = "text_differs".to_string();
                    score += ELEMENT_TEXT_WEIGHT;
                } else if delta.iter().any(|d| i64::from(d.abs()) > ELEMENT_DELTA_FLOOR)
                    || entry
                        .size_delta
                        .unwrap()
                        .iter()
                        .any(|d| i64::from(d.abs()) > ELEMENT_DELTA_FLOOR)
                {
                    entry.kind = "bounds_differ".to_string();
                }
                entry.score = score;
            }
            (None, None) => continue,
        }
        out.push(entry);
    }
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.resource_id.cmp(&b.resource_id))
    });
    out
}

/// One normalized difference between the device reference and ACOS.
#[derive(Debug, Clone, Serialize)]
pub struct Difference {
    /// `missing_in_acos`, `extra_in_acos`, `text_differs`, `class_differs`,
    /// `bounds_differ`, `state_differs`.
    pub kind: String,
    /// Stable matching key (`id:<resource-id>` / `text:<class>|<text>` /
    /// `class:<class>`).
    pub key: String,
    /// Android resource id, if any.
    pub resource_id: String,
    /// View class, if any.
    pub class: String,
    /// Node text, if any.
    pub text: String,
    /// Reference (device) bounds, if the node exists there.
    pub device_bounds: Option<[i32; 4]>,
    /// ACOS bounds, if the node exists there.
    pub acos_bounds: Option<[i32; 4]>,
    /// Per-edge `acos - scaled_device` delta, when both bounds exist.
    pub delta: Option<[i32; 4]>,
    /// Human-readable classification (`bounds_differ_density`, ...).
    pub note: String,
}

/// The comparator result.
#[derive(Debug, Clone, Serialize)]
pub struct ParityReport {
    /// Fully matching nodes.
    pub matches: u64,
    /// Every normalized difference, in stable key order.
    pub differences: Vec<Difference>,
    /// Number of nodes in the device/reference tree.
    pub device_nodes: usize,
    /// Number of nodes in the ACOS tree.
    pub acos_nodes: usize,
    /// True when there is no difference at all.
    pub identical: bool,
    /// Ranked per-element (resource-id) layout differences, when requested.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub element_diff: Vec<ElementDifference>,
    /// The options applied.
    pub options: Value,
}

/// Normalize a node to its matching key.
/// The Flutter frame preset: reference device screenshot vs an ACOS frame,
/// masking the system status and navigation bars.
///
/// The reference and the ACOS frame are both 1080x2400 at 400 dpi. The mask
/// rectangles come from the W130-A measurement: rows `0..108` are the opaque
/// status band and rows `2304..2400` the navigation band. The pass threshold is
/// the same the W130-A harness asserts (`global_ssim >= 0.98` and
/// `mean_ssim >= 0.99`).
fn flutter_frame_parity(candidate: &Path) -> Value {
    let reference = repo_root().join("fixtures/reference-device/flutter-hello/screenshot_initial.png");
    frame_parity(&reference, candidate, &[[0, 0, 1080, 108], [0, 2304, 1080, 2400]])
}

/// The framework-app frame preset: the ART reference screenshot for a fixture
/// app vs an ACOS frame, masking the status and navigation bars.
///
/// The reference is `fixtures/reference/<app>/art-api34-x86_64/screenshot_initial.png`
/// (1080x2400 @ 420 dpi). The status bar is 24 dp (`24 * 420 / 160 = 63` px,
/// rounded to 64); the navigation band is the W130-A measured `2304..2400`.
fn framework_frame_parity(app: &str, candidate: &Path) -> Value {
    let reference = repo_root()
        .join("fixtures/reference")
        .join(app)
        .join("art-api34-x86_64/screenshot_initial.png");
    if !reference.is_file() {
        return json!({
            "ok": false,
            "code": "RENDER_INVALID_ARGUMENT",
            "message": format!(
                "no ART reference screenshot for app {app:?} at {}; \
                 pass `--recorded <app>` with one of the fixtures that have a reference",
                reference.display()
            ),
        });
    }
    frame_parity(&reference, candidate, &[[0, 0, 1080, 64], [0, 2304, 1080, 2400]])
}

/// Compare `candidate` against `reference`, masking `masks`, and report the
/// same SSIM verdict the W130-A harness asserts.
fn frame_parity(reference: &Path, candidate: &Path, masks: &[[u32; 4]]) -> Value {
    use acos_render::diff::{self, DiffOptions};
    use acos_render::image;

    let load = |path: &Path| -> Result<image::DecodedImage, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        image::decode(&bytes).map_err(|e| format!("{}: {e}", path.display()))
    };
    let (reference_img, candidate_img) = match (load(reference), load(candidate)) {
        (Ok(r), Ok(c)) => (r, c),
        (Err(e), _) | (_, Err(e)) => {
            return json!({"ok": false, "code": "RENDER_INVALID_ARGUMENT", "message": e})
        }
    };
    let opts = DiffOptions {
        pixel_threshold: 8,
        ssim_window: 8,
        block_size: 8,
        mask: masks.to_vec(),
    };
    match diff::diff(&reference_img, &candidate_img, &opts) {
        Ok(report) => {
            let identical = report.global_ssim >= 0.98 && report.mean_ssim >= 0.99;
            json!({
                "ok": true,
                "identical": identical,
                "reference": reference.display().to_string(),
                "candidate": candidate.display().to_string(),
                "masks": masks,
                "threshold": {"global_ssim": 0.98, "mean_ssim": 0.99},
                "diff": report,
            })
        }
        Err(e) => json!({"ok": false, "code": "RENDER_INVALID_ARGUMENT", "message": e.to_string()}),
    }
}

fn match_key(node: &ViewNode) -> String {
    if !node.resource_id.is_empty() {
        format!("id:{}", node.resource_id)
    } else if !node.text.trim().is_empty() {
        format!("text:{}|{}", node.class, node.text.trim())
    } else {
        format!("class:{}", node.class)
    }
}

fn group(nodes: &[ViewNode]) -> BTreeMap<String, Vec<&ViewNode>> {
    let mut map: BTreeMap<String, Vec<&ViewNode>> = BTreeMap::new();
    for node in flatten(nodes) {
        map.entry(match_key(node)).or_default().push(node);
    }
    map
}

/// Compare the device reference tree against the ACOS tree.
pub fn compare(device: &[ViewNode], acos: &[ViewNode], opts: CompareOptions) -> ParityReport {
    let device_keys = group(device);
    let acos_keys = group(acos);
    let mut keys: Vec<&String> = device_keys.keys().chain(acos_keys.keys()).collect();
    keys.sort();
    keys.dedup();

    let mut matches = 0u64;
    let mut differences = Vec::new();
    for key in keys {
        let empty = Vec::new();
        let ds = device_keys.get(key).unwrap_or(&empty);
        let as_ = acos_keys.get(key).unwrap_or(&empty);
        for i in 0..ds.len().max(as_.len()) {
            match (ds.get(i), as_.get(i)) {
                (Some(d), Some(a)) => {
                    let mut diff = compare_pair(key, d, a, opts);
                    if diff.is_empty() {
                        matches += 1;
                    } else {
                        differences.append(&mut diff);
                    }
                }
                (Some(d), None) => differences.push(Difference {
                    kind: "missing_in_acos".to_string(),
                    key: key.clone(),
                    resource_id: d.resource_id.clone(),
                    class: d.class.clone(),
                    text: d.text.clone(),
                    device_bounds: Some(d.bounds),
                    acos_bounds: None,
                    delta: None,
                    note: "device exposes the node; ACOS does not".to_string(),
                }),
                (None, Some(a)) => differences.push(Difference {
                    kind: "extra_in_acos".to_string(),
                    key: key.clone(),
                    resource_id: a.resource_id.clone(),
                    class: a.class.clone(),
                    text: a.text.clone(),
                    device_bounds: None,
                    acos_bounds: Some(a.bounds),
                    delta: None,
                    note: "ACOS exposes the node; the device does not".to_string(),
                }),
                (None, None) => {}
            }
        }
    }

    let identical = differences.is_empty();
    let element_diff = if opts.element_diff {
        element_differences(acos, device)
    } else {
        Vec::new()
    };
    ParityReport {
        matches,
        differences,
        device_nodes: flatten(device).len(),
        acos_nodes: flatten(acos).len(),
        identical,
        element_diff,
        options: json!({
            "bounds_tolerance": opts.bounds_tolerance,
            "bounds_scale": opts.bounds_scale,
            "ignore_bounds": opts.ignore_bounds,
            "strict_state": opts.strict_state,
            "element_diff": opts.element_diff,
        }),
    }
}

fn compare_pair(
    key: &str,
    device: &ViewNode,
    acos: &ViewNode,
    opts: CompareOptions,
) -> Vec<Difference> {
    let mut out = Vec::new();
    let base = |kind: &str, note: String| Difference {
        kind: kind.to_string(),
        key: key.to_string(),
        resource_id: if acos.resource_id.is_empty() {
            device.resource_id.clone()
        } else {
            acos.resource_id.clone()
        },
        class: acos.class.clone(),
        text: acos.text.clone(),
        device_bounds: Some(device.bounds),
        acos_bounds: Some(acos.bounds),
        delta: None,
        note,
    };

    if device.text.trim() != acos.text.trim() {
        out.push(base(
            "text_differs",
            format!(
                "device {:?} vs acos {:?}",
                device.text.trim(),
                acos.text.trim()
            ),
        ));
    }
    if !device.class.is_empty() && !acos.class.is_empty() && device.class != acos.class {
        out.push(base(
            "class_differs",
            format!("device {} vs acos {}", device.class, acos.class),
        ));
    }
    if opts.strict_state {
        let state = |a: bool, b: bool, label: &str, out: &mut Vec<Difference>| {
            if a != b {
                out.push(base("state_differs", format!("{label}: device {a} vs acos {b}")));
            }
        };
        state(device.enabled, acos.enabled, "enabled", &mut out);
        state(device.focused, acos.focused, "focused", &mut out);
        state(device.clickable, acos.clickable, "clickable", &mut out);
    }
    if !opts.ignore_bounds {
        let scaled = scale_bounds(device.bounds, opts.bounds_scale);
        let delta = [
            acos.bounds[0] - scaled[0],
            acos.bounds[1] - scaled[1],
            acos.bounds[2] - scaled[2],
            acos.bounds[3] - scaled[3],
        ];
        let within = delta
            .iter()
            .all(|d| d.abs() <= opts.bounds_tolerance);
        if !within {
            let mut diff = base(
                "bounds_differ",
                if opts.bounds_scale != 1.0 {
                    "bounds_differ_density".to_string()
                } else {
                    "bounds differ beyond tolerance".to_string()
                },
            );
            diff.delta = Some(delta);
            out.push(diff);
        }
    }
    out
}

fn scale_bounds(bounds: [i32; 4], scale: f64) -> [i32; 4] {
    if scale == 1.0 {
        return bounds;
    }
    [
        (bounds[0] as f64 * scale).round() as i32,
        (bounds[1] as f64 * scale).round() as i32,
        (bounds[2] as f64 * scale).round() as i32,
        (bounds[3] as f64 * scale).round() as i32,
    ]
}

/// Frame parity preset: `framework` (ART reference for a fixture app) or
/// `flutter` (the reference device capture).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum FramePreset {
    /// Framework-app preset: the ART reference for `--recorded <app>`, which
    /// defaults to `counter`. Masks the status bar (`0..64`, 24 dp at 420 dpi)
    /// and the navigation band (`2304..2400`).
    Framework,
    /// Flutter preset: `fixtures/reference-device/flutter-hello` with the
    /// W130-A status (`0..108`) and navigation (`2304..2400`) bands masked.
    Flutter,
}

/// Arguments for `acos parity`.
#[derive(Debug, clap::Args)]
pub struct ParityArgs {
    /// adb serial of the phone to drive (for example `ZA222WGJPP`). Omit for
    /// the offline `--recorded` mode.
    #[arg(long)]
    pub device: Option<String>,

    /// APK to install and launch on the device. Required with `--device`.
    pub apk: Option<PathBuf>,

    /// Offline: compare the recorded captures for this fixture app
    /// (`fixtures/reference-device/<app>` against the ART reference).
    #[arg(long)]
    pub recorded: Option<String>,

    /// Offline: a stored ACOS `observe --tree` JSON to use as the ACOS side.
    #[arg(long)]
    pub acos_tree: Option<PathBuf>,

    /// Offline: a specific device snapshot JSON to compare (defaults to
    /// `snapshot-00-launch.json`).
    #[arg(long)]
    pub device_snapshot: Option<PathBuf>,

    /// Maximum per-edge bounds delta after scaling.
    #[arg(long, default_value_t = 4)]
    pub bounds_tolerance: i32,

    /// Scale applied to the device bounds before comparing (density ratio).
    #[arg(long, default_value_t = 1.0)]
    pub bounds_scale: f64,

    /// Compare text/class/id only, ignoring bounds.
    #[arg(long)]
    pub ignore_bounds: bool,

    /// Also compare enabled/focused/clickable.
    #[arg(long)]
    pub strict_state: bool,

    /// Emit the ranked per-element (resource-id) layout diff: bounds delta,
    /// text and size for each app view, largest real difference first.
    #[arg(long = "element-diff")]
    pub element_diff: bool,

    /// Directory for the captured device screenshots/XML (live mode).
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Keep the fixture APK installed after the live run.
    #[arg(long)]
    pub keep: bool,

    /// Flutter preset: compare this ACOS frame PNG against
    /// `fixtures/reference-device/flutter-hello/screenshot_initial.png` with
    /// the status bar (rows `0..108`) and navigation bar (`2304..2400`) masked.
    #[arg(long = "flutter-frame")]
    pub flutter_frame: Option<PathBuf>,

    /// Frame parity preset (analogous to `--flutter-frame`): compare the PNG in
    /// `--frame` against the preset reference with the system bars masked.
    #[arg(long = "frame-preset", value_enum)]
    pub frame_preset: Option<FramePreset>,

    /// Candidate ACOS frame PNG for `--frame-preset`.
    #[arg(long)]
    pub frame: Option<PathBuf>,

    /// Emit the JSON report (always true in practice; kept for symmetry).
    #[arg(long)]
    pub json: bool,
}

/// Run `acos parity`.
pub fn run(args: ParityArgs) -> ExitCode {
    if let Some(preset) = args.frame_preset {
        let Some(candidate) = args.frame.clone() else {
            let report = json!({
                "ok": false,
                "code": "INVALID_ARGUMENT",
                "message": "`--frame-preset` needs a candidate PNG via `--frame <path>`",
            });
            crate::print_json(&report);
            return ExitCode::FAILURE;
        };
        let report = match preset {
            FramePreset::Flutter => flutter_frame_parity(&candidate),
            FramePreset::Framework => framework_frame_parity(
                args.recorded.as_deref().unwrap_or("counter"),
                &candidate,
            ),
        };
        let identical = report["identical"].as_bool().unwrap_or(false);
        if args.json {
            crate::print_json(&report);
        } else {
            println!(
                "frame parity ({}): identical={identical} global_ssim={:.4} mean_ssim={:.4} differing={:.5}",
                args.recorded.as_deref().unwrap_or("counter"),
                report["diff"]["global_ssim"].as_f64().unwrap_or(0.0),
                report["diff"]["mean_ssim"].as_f64().unwrap_or(0.0),
                report["diff"]["differing_fraction"].as_f64().unwrap_or(0.0),
            );
        }
        return if identical {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    if let Some(frame) = args.flutter_frame.clone() {
        let report = flutter_frame_parity(&frame);
        let identical = report["identical"].as_bool().unwrap_or(false);
        if args.json {
            crate::print_json(&report);
        } else {
            println!(
                "flutter parity: identical={identical} global_ssim={:.4} mean_ssim={:.4} differing={:.5}",
                report["diff"]["global_ssim"].as_f64().unwrap_or(0.0),
                report["diff"]["mean_ssim"].as_f64().unwrap_or(0.0),
                report["diff"]["differing_fraction"].as_f64().unwrap_or(0.0),
            );
        }
        return if identical {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let opts = CompareOptions {
        bounds_tolerance: args.bounds_tolerance,
        bounds_scale: args.bounds_scale,
        ignore_bounds: args.ignore_bounds,
        strict_state: args.strict_state,
        element_diff: args.element_diff,
    };

    let report = match (&args.device, &args.recorded, &args.acos_tree) {
        (Some(serial), None, _) => live_parity(serial, args.apk.as_deref(), opts, &args),
        (None, Some(app), _) => recorded_parity(app, args.acos_tree.as_deref(), args.device_snapshot.as_deref(), opts),
        (None, None, Some(_)) => recorded_parity("", args.acos_tree.as_deref(), args.device_snapshot.as_deref(), opts),
        (None, None, None) => Err(ParityFailure::usage(
            "specify `--device <serial> <apk>` for a live run or `--recorded <app>` for an offline one",
        )),
        (Some(_), Some(_), _) => Err(ParityFailure::usage(
            "`--device` and `--recorded` are mutually exclusive",
        )),
    };

    match report {
        Ok(report) => {
            let identical = report
                .get("identical")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Some(path) = &args.output {
                let _ = std::fs::create_dir_all(path);
                let _ = std::fs::write(
                    path.join("parity.json"),
                    serde_json::to_vec_pretty(&report).unwrap_or_default(),
                );
            }
            if args.json {
                crate::print_json(&report);
            } else {
                println!(
                    "parity: identical={identical} matches={} differences={}",
                    report.get("matches").and_then(Value::as_u64).unwrap_or(0),
                    report
                        .get("differences")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0)
                );
                if let Some(rows) = report.get("element_diff").and_then(Value::as_array) {
                    println!("ranked element diff (acos - phone):");
                    for row in rows.iter().take(20) {
                        let id = row.get("resource_id").and_then(Value::as_str).unwrap_or("");
                        let kind = row.get("kind").and_then(Value::as_str).unwrap_or("");
                        let score = row.get("score").and_then(Value::as_i64).unwrap_or(0);
                        let a = bounds_str(row.get("acos_bounds"));
                        let p = bounds_str(row.get("phone_bounds"));
                        let d = bounds_str(row.get("delta"));
                        let s = pair_str(row.get("size_delta"));
                        println!(
                            "  {score:>7} {kind:<16} {id}\n            acos={a} phone={p} delta={d} size_delta={s}"
                        );
                    }
                }
            }
            if identical {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(failure) => {
            crate::print_json(&failure.to_json());
            ExitCode::FAILURE
        }
    }
}


/// A loud, structured skip/usage failure.
#[derive(Debug, Clone, Serialize)]
pub struct ParityFailure {
    /// `ok` is always `false` here.
    pub ok: bool,
    /// Stable ACOS error code (`NEEDS_REAL_DEVICE`, `INVALID_ARGUMENT`, ...).
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Machine-readable details.
    pub details: Value,
}

impl ParityFailure {
    fn usage(message: impl Into<String>) -> Self {
        ParityFailure {
            ok: false,
            code: "INVALID_ARGUMENT".to_string(),
            message: message.into(),
            details: json!({}),
        }
    }

    fn no_device(message: impl Into<String>, details: Value) -> Self {
        ParityFailure {
            ok: false,
            code: "NEEDS_REAL_DEVICE".to_string(),
            message: message.into(),
            details,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "ok": self.ok,
            "mode": "parity",
            "code": self.code,
            "message": self.message,
            "details": self.details,
        })
    }
}

fn bounds_str(value: Option<&Value>) -> String {
    match value.and_then(Value::as_array) {
        Some(a) => format!(
            "[{},{},{},{}]",
            a.first().and_then(Value::as_i64).unwrap_or(0),
            a.get(1).and_then(Value::as_i64).unwrap_or(0),
            a.get(2).and_then(Value::as_i64).unwrap_or(0),
            a.get(3).and_then(Value::as_i64).unwrap_or(0),
        ),
        None => "-".to_string(),
    }
}

fn pair_str(value: Option<&Value>) -> String {
    match value.and_then(Value::as_array) {
        Some(a) => format!(
            "[{},{}]",
            a.first().and_then(Value::as_i64).unwrap_or(0),
            a.get(1).and_then(Value::as_i64).unwrap_or(0),
        ),
        None => "-".to_string(),
    }
}

fn recorded_json(report: &ParityReport, app: &str, device_dir: &Path, acos_source: &str) -> Value {
    let mut value = serde_json::to_value(report).unwrap_or(Value::Null);
    if let Value::Object(ref mut map) = value {
        map.insert("ok".to_string(), json!(report.identical));
        map.insert("mode".to_string(), json!("recorded"));
        map.insert("fidelity".to_string(), json!("physical-device"));
        map.insert("app".to_string(), json!(app));
        map.insert("device_dir".to_string(), json!(device_dir.display().to_string()));
        map.insert("acos_source".to_string(), json!(acos_source));
    }
    value
}

/// Offline: compare a recorded device capture against the ART reference (or a
/// stored ACOS tree when `--acos-tree` is given).
fn recorded_parity(
    app: &str,
    acos_tree: Option<&Path>,
    device_snapshot: Option<&Path>,
    opts: CompareOptions,
) -> Result<Value, ParityFailure> {
    let root = repo_root();
    let device_dir = if app.is_empty() {
        root.join("fixtures/reference-device")
    } else {
        root.join("fixtures/reference-device").join(app)
    };
    let snapshot_path = device_snapshot
        .map(Path::to_path_buf)
        .unwrap_or_else(|| device_dir.join("snapshot-00-launch.json"));
    let device_text = std::fs::read_to_string(&snapshot_path).map_err(|e| {
        // A missing app directory is the common case (the fixture was never
        // recorded on a phone); naming the snapshot file alone reads as a
        // corrupt-file error and gives the agent no next step.
        if !app.is_empty() && !device_dir.is_dir() {
            let mut available: Vec<String> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(root.join("fixtures/reference-device")) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                            available.push(name.to_string());
                        }
                    }
                }
            }
            available.sort();
            let hint = if available.is_empty() {
                "no recorded device captures are present".to_string()
            } else {
                format!("available recorded apps: {}", available.join(", "))
            };
            ParityFailure::usage(format!(
                "no recorded device capture for app {app:?} ({} does not exist); {hint}. \
                 Use `--device-snapshot <path>` for a capture you have, or fall back to the \
                 ART reference without `--recorded`.",
                device_dir.display()
            ))
        } else {
            ParityFailure::usage(format!(
                "recorded device snapshot {} is unreadable: {e}",
                snapshot_path.display()
            ))
        }
    })?;
    let snapshot_json: Value = serde_json::from_str(&device_text).map_err(|e| {
        ParityFailure::usage(format!("snapshot {} is not JSON: {e}", snapshot_path.display()))
    })?;
    let device_nodes = parse_agent_device_snapshot(&snapshot_json).map_err(ParityFailure::usage)?;

    let (acos_nodes, acos_source) = match acos_tree {
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| {
                ParityFailure::usage(format!("ACOS tree {} is unreadable: {e}", path.display()))
            })?;
            let value: Value = serde_json::from_str(&text).map_err(|e| {
                ParityFailure::usage(format!("ACOS tree {} is not JSON: {e}", path.display()))
            })?;
            (
                parse_acos_tree_json(&value).map_err(ParityFailure::usage)?,
                path.display().to_string(),
            )
        }
        None => {
            // Fall back to the API-34 ART reference. This is explicitly the
            // emulator tree, not an ACOS capture; the report says so.
            let reference = root
                .join("fixtures/reference")
                .join(if app.is_empty() { "counter" } else { app })
                .join("art-api34-x86_64/ui_initial.xml");
            let text = std::fs::read_to_string(&reference).map_err(|e| {
                ParityFailure::usage(format!(
                    "no --acos-tree and ART reference {} is unreadable: {e}",
                    reference.display()
                ))
            })?;
            (
                parse_uiautomator_xml(&text).map_err(ParityFailure::usage)?,
                reference.display().to_string(),
            )
        }
    };

    let report = compare(&device_nodes, &acos_nodes, opts);
    Ok(recorded_json(&report, app, &device_dir, &acos_source))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."))
}

// ---------------------------------------------------------------------------
// live path
// ---------------------------------------------------------------------------

fn adb(serial: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new("adb")
        .arg("-s")
        .arg(serial)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run adb: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "adb {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// A loud, structured skip when the phone cannot supply a UI capture (busy or
/// out of memory). It still persists the screenshot as a reference so a later
/// run has *some* evidence, but never claims a comparison.
#[allow(clippy::too_many_arguments)]
fn skip_report(
    serial: &str,
    package: &str,
    app: &str,
    output_dir: &Path,
    reference_dir: &Path,
    screenshot: &[u8],
    message: &str,
) -> Value {
    let mut saved: Vec<String> = Vec::new();
    if !screenshot.is_empty() {
        let _ = std::fs::create_dir_all(reference_dir);
        let path = reference_dir.join("device-screenshot.png");
        if std::fs::write(&path, screenshot).is_ok() {
            saved.push(path.display().to_string());
        }
        let _ = std::fs::write(output_dir.join("device-screenshot.png"), screenshot);
    }
    json!({
        "ok": false,
        "mode": "physical-device",
        "fidelity": "physical-device",
        "code": "DEVICE_BUSY",
        "device": serial,
        "package": package,
        "app": app,
        "message": message,
        "output": output_dir.display().to_string(),
        "reference_dir": reference_dir.display().to_string(),
        "saved": saved,
        "device_nodes": 0,
        "acos_nodes": 0,
        "matches": 0,
        "identical": false,
        "differences": [],
    })
}

fn live_parity(
    serial: &str,
    apk: Option<&Path>,
    opts: CompareOptions,
    args: &ParityArgs,
) -> Result<Value, ParityFailure> {
    let Some(apk) = apk else {
        return Err(ParityFailure::usage(
            "`--device <serial>` needs an APK path: `acos parity --device ZA222WGJPP app.apk`",
        ));
    };
    let apk = apk.canonicalize().map_err(|e| {
        ParityFailure::usage(format!("APK {} is unreadable: {e}", apk.display()))
    })?;

    // Determine the package and refuse anything that is not ours.
    let manifest = acos_apk::Apk::open(&apk)
        .and_then(|apk| apk.manifest())
        .map_err(|e| ParityFailure::usage(format!("cannot inspect {}: {e}", apk.display())))?;
    let package = manifest.package.clone();
    if !package.starts_with("dev.acos.fixtures.") {
        return Err(ParityFailure {
            ok: false,
            code: "INVALID_ARGUMENT".to_string(),
            message: format!(
                "refusing to install {package}: parity only drives dev.acos.fixtures.* packages"
            ),
            details: json!({"package": package}),
        });
    }
    let activity = manifest
        .launcher_activity()
        .map(|c| format!("{package}/{}", c.name))
        .unwrap_or_else(|| package.clone());

    if adb(serial, &["get-state"]).is_err() {
        return Err(ParityFailure::no_device(
            format!("device {serial} is not attached; parity skipped (no false pass)"),
            json!({"device": serial, "package": package}),
        ));
    }

    let output_dir = args
        .output
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("acos-parity-{package}")));
    let _ = std::fs::create_dir_all(&output_dir);

    adb(serial, &["install", "-r", "-g", &apk.display().to_string()])
        .or_else(|first| {
            // A previous capture may have installed the same fixture package
            // signed with an older throwaway debug keystore. Only our own
            // `dev.acos.fixtures.*` package can reach here; uninstall it and
            // retry rather than failing the whole run.
            if first.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE")
                || first.contains("signatures do not match")
            {
                let _ = adb(serial, &["uninstall", &package]);
                adb(serial, &["install", "-r", "-g", &apk.display().to_string()])
            } else {
                Err(first)
            }
        })
        .map_err(|e| ParityFailure::no_device(format!("install failed: {e}"), json!({})))?;
    let _ = adb(serial, &["shell", "am", "force-stop", &package]);
    adb(
        serial,
        &["shell", "am", "start", "-W", "-n", &activity],
    )
    .map_err(|e| ParityFailure::no_device(format!("launch failed: {e}"), json!({})))?;

    // Phone capture: screenshot first (screencap survives device memory
    // pressure better than uiautomator), then the uiautomator XML.
    let screenshot: Vec<u8> = Command::new("adb")
        .args(["-s", serial, "exec-out", "screencap", "-p"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout)
        .unwrap_or_default();
    if !screenshot.is_empty() {
        std::fs::write(output_dir.join("device-screenshot.png"), &screenshot).ok();
    }

    let app = apk
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| package.clone());
    let reference_dir = repo_root().join("fixtures/reference-device").join(&app);

    let remote_xml = "/sdcard/acos_parity_window_dump.xml";
    let _ = adb(serial, &["shell", "rm", "-f", remote_xml]);
    // `uiautomator dump` can be SIGKILLed by the device's low-memory killer
    // *after* it has written the file, so the exit status is not trustworthy;
    // read the file and require a real `<hierarchy>` before using it.
    let _ = adb(serial, &["shell", "uiautomator", "dump", remote_xml]);
    let xml = adb(serial, &["exec-out", "cat", remote_xml])
        .ok()
        .filter(|text| text.contains("<hierarchy"))
        .unwrap_or_default();

    if xml.is_empty() {
        // uiautomator was killed (commonly OOM on a memory-pressured device)
        // or produced no hierarchy. Do not fabricate a capture: uninstall, keep
        // the screenshot, and report a loud structured skip.
        if !args.keep {
            let _ = adb(serial, &["uninstall", &package]);
        }
        return Ok(skip_report(
            serial,
            &package,
            &app,
            &output_dir,
            &reference_dir,
            &screenshot,
            "uiautomator dump produced no <hierarchy>; the device may be out of memory \
             (check `adb shell free -m`) or busy; parity skipped (no false pass)",
        ));
    }

    std::fs::write(output_dir.join("device-ui.xml"), &xml).ok();
    let device_nodes = parse_uiautomator_xml(&xml).map_err(ParityFailure::usage)?;

    // Persist the phone capture as a checked-in reference for this fixture app
    // (`fixtures/reference-device/<app>/`). Only the small XML + PNG are kept.
    let mut saved: Vec<String> = Vec::new();
    let _ = std::fs::create_dir_all(&reference_dir);
    for (name, bytes) in [
        ("device-ui.xml", xml.as_bytes().to_vec()),
        ("device-screenshot.png", screenshot.clone()),
    ] {
        if bytes.is_empty() {
            continue;
        }
        let path = reference_dir.join(name);
        if std::fs::write(&path, &bytes).is_ok() {
            saved.push(path.display().to_string());
        }
    }

    // ACOS side: boot with the **real** boot classpath (the same options the
    // session launcher uses) and walk the live View hierarchy. The bootstrap
    // corelib cannot even find `android.os.Looper`, so `Runtime::standard()`
    // would fail before reaching the app.
    let options = VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    };
    let mut runtime = acos_runtime::Runtime::standard_with(options).map_err(|e| {
        ParityFailure::no_device(format!("cannot boot ACOS runtime: {e}"), json!({}))
    })?;
    let _ = runtime.register_app_natives(&apk);
    let launch = runtime.boot_and_run_lifecycle(&apk);
    let acos_nodes = match acos_runtime::view_tree::walk(runtime.vm_mut(), 2000) {
        Ok(tree) => {
            std::fs::write(
                output_dir.join("acos-tree.json"),
                serde_json::to_vec_pretty(&tree_to_json(&tree)).unwrap_or_default(),
            )
            .ok();
            tree.nodes
        }
        Err(e) => {
            if !args.keep {
                let _ = adb(serial, &["uninstall", &package]);
            }
            // ACOS cannot produce its side yet (today it stops in the launch
            // ladder, e.g. at `VectorDrawable`). Emit a structured
            // `acos_blocked_at` report that keeps the phone capture and lists
            // every device node as `missing_in_acos`; never an empty pass and
            // never a bare `NEEDS_REAL_DEVICE` that discards the capture.
            let partial = compare(&device_nodes, &[], opts);
            let mut value = serde_json::to_value(&partial).unwrap_or(Value::Null);
            if let Value::Object(ref mut map) = value {
                map.insert("ok".to_string(), json!(false));
                map.insert("mode".to_string(), json!("physical-device"));
                map.insert("fidelity".to_string(), json!("physical-device"));
                map.insert("code".to_string(), json!("ACOS_BLOCKED"));
                map.insert("device".to_string(), json!(serial));
                map.insert("package".to_string(), json!(package));
                map.insert("app".to_string(), json!(app));
                map.insert("output".to_string(), json!(output_dir.display().to_string()));
                map.insert("reference_dir".to_string(), json!(reference_dir.display().to_string()));
                map.insert("saved".to_string(), json!(saved));
                map.insert(
                    "message".to_string(),
                    json!(format!(
                        "ACOS could not walk its View hierarchy ({}): {}; the phone capture was \
                         saved as a reference and no comparison is claimed",
                        e.reason(),
                        e.message(),
                    )),
                );
                map.insert(
                    "acos_blocked_at".to_string(),
                    json!({
                        "reason": e.reason(),
                        "message": e.message(),
                        "app_created": launch.app_created,
                        "transaction_delivered": launch.transaction_delivered,
                        "looper_quit": launch.looper_quit,
                        "launch_error": launch.error,
                    }),
                );
            }
            return Ok(value);
        }
    };

    let report = compare(&device_nodes, &acos_nodes, opts);

    if !args.keep {
        let _ = adb(serial, &["uninstall", &package]);
    }

    let mut value = serde_json::to_value(&report).unwrap_or(Value::Null);
    if let Value::Object(ref mut map) = value {
        map.insert("ok".to_string(), json!(report.identical));
        map.insert("mode".to_string(), json!("physical-device"));
        map.insert("fidelity".to_string(), json!("physical-device"));
        map.insert("device".to_string(), json!(serial));
        map.insert("package".to_string(), json!(package));
        map.insert("app".to_string(), json!(app));
        map.insert("output".to_string(), json!(output_dir.display().to_string()));
        map.insert("reference_dir".to_string(), json!(reference_dir.display().to_string()));
        map.insert("saved".to_string(), json!(saved));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe_tree::{parse_agent_device_snapshot, parse_uiautomator_xml};

    /// Counter: the recorded phone capture (API 35, 400 dpi) vs the API-34
    /// 420 dpi ART reference. Texts and app ids must match; bounds differ only
    /// by the density/inset ratio, which the comparator classifies rather than
    /// reporting as a text failure.
    #[test]
    fn recorded_counter_matches_text_and_classifies_bounds() {
        let root = repo_root();
        let device: Value = serde_json::from_str(
            &std::fs::read_to_string(
                root.join("fixtures/reference-device/counter/snapshot-00-launch.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let reference = std::fs::read_to_string(
            root.join("fixtures/reference/counter/art-api34-x86_64/ui_initial.xml"),
        )
        .unwrap();
        let device_nodes = parse_agent_device_snapshot(&device).unwrap();
        let reference_nodes = parse_uiautomator_xml(&reference).unwrap();

        let opts = CompareOptions {
            bounds_tolerance: 16,
            bounds_scale: 420.0 / 400.0,
            ..CompareOptions::default()
        };
        let report = compare(&device_nodes, &reference_nodes, opts);

        // The app's own ids and their text must match exactly.
        for id in [
            "dev.acos.fixtures.counter:id/count",
            "dev.acos.fixtures.counter:id/inc",
            "dev.acos.fixtures.counter:id/reset",
        ] {
            let missing = report
                .differences
                .iter()
                .find(|d| d.resource_id == id && d.kind != "bounds_differ");
            assert!(
                missing.is_none(),
                "unexpected non-bounds difference for {id}: {missing:?}"
            );
        }
        // Bounds differences are classified as density, not text, differences.
        assert!(
            report
                .differences
                .iter()
                .all(|d| d.kind == "bounds_differ" || d.kind == "missing_in_acos" || d.kind == "extra_in_acos"),
            "unexpected difference kinds: {:?}",
            report.differences
        );
        assert!(
            report
                .differences
                .iter()
                .filter(|d| d.kind == "bounds_differ")
                .all(|d| d.note == "bounds_differ_density"),
            "bounds differences must be labelled density"
        );
        assert!(report.matches > 0);
    }

    /// The ranked per-element diff matches by resource id and ranks the
    /// largest real layout difference first; matching elements are `matched`.
    #[test]
    fn element_diff_ranks_by_resource_id() {
        let acos = parse_uiautomator_xml(
            r#"<hierarchy>
                <node text="Increment" resource-id="pkg:id/inc" class="android.widget.Button" bounds="[84,634][353,760]"/>
                <node text="Reset" resource-id="pkg:id/reset" class="android.widget.Button" bounds="[84,886][315,1012]"/>
              </hierarchy>"#,
        )
        .unwrap();
        let phone = parse_uiautomator_xml(
            r#"<hierarchy>
                <node text="Increment" resource-id="pkg:id/inc" class="android.widget.Button" bounds="[40,334][262,454]"/>
                <node text="Reset" resource-id="pkg:id/reset" class="android.widget.Button" bounds="[84,886][315,1012]"/>
              </hierarchy>"#,
        )
        .unwrap();
        let ranked = element_differences(&acos, &phone);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].resource_id, "pkg:id/inc");
        assert_eq!(ranked[0].kind, "bounds_differ");
        assert_eq!(ranked[0].delta, Some([44, 300, 91, 306]));
        assert_eq!(ranked[0].acos_size, Some([269, 126]));
        assert_eq!(ranked[0].phone_size, Some([222, 120]));
        assert_eq!(ranked[0].size_delta, Some([47, 6]));
        // The identical Reset element ranks last and is `matched`.
        assert_eq!(ranked[1].resource_id, "pkg:id/reset");
        assert_eq!(ranked[1].kind, "matched");
        assert_eq!(ranked[1].score, 0);
        assert!(ranked[0].score > ranked[1].score);
    }

    /// A missing element is text-level, not geometry, and outranks a shift.
    #[test]
    fn element_diff_flags_missing_and_text() {
        let acos = parse_uiautomator_xml(
            r#"<hierarchy><node text="Increment" resource-id="pkg:id/inc" class="android.widget.Button" bounds="[0,0][10,10]"/></hierarchy>"#,
        )
        .unwrap();
        let phone = parse_uiautomator_xml(
            r#"<hierarchy><node text="Count" resource-id="pkg:id/count" class="android.widget.TextView" bounds="[0,0][10,10]"/></hierarchy>"#,
        )
        .unwrap();
        let ranked = element_differences(&acos, &phone);
        assert_eq!(ranked.len(), 2);
        assert!(ranked.iter().any(|e| e.kind == "missing_in_acos"));
        assert!(ranked.iter().any(|e| e.kind == "missing_on_phone"));
        assert!(ranked.iter().all(|e| e.score >= 100_000));
    }

    /// A busy phone skip is structured, not a pass.
    #[test]
    fn busy_skip_is_structured_not_a_pass() {
        let dir = std::env::temp_dir().join("acos-parity-skip-test");
        let report = skip_report(
            "SERIAL",
            "dev.acos.fixtures.counter",
            "counter",
            &dir,
            &dir,
            &[],
            "the device is busy",
        );
        assert_eq!(report["ok"], false);
        assert_eq!(report["code"], "DEVICE_BUSY");
        assert_eq!(report["identical"], false);
        assert!(report["message"].as_str().unwrap().contains("busy"));
    }

    /// A one-pixel change is caught; the same tree compares identical.
    #[test]
    fn identical_trees_have_no_differences() {
        let xml = r#"<hierarchy><node text="Hello" resource-id="pkg:id/t" class="android.widget.TextView" package="pkg" bounds="[10,20][30,40]" enabled="true" focused="false" clickable="false" scrollable="false" checked="false"/></hierarchy>"#;
        let nodes = parse_uiautomator_xml(xml).unwrap();
        let report = compare(&nodes, &nodes, CompareOptions::default());
        assert!(report.identical, "{:?}", report.differences);
        assert_eq!(report.matches, 1);
    }

    #[test]
    fn text_and_bounds_differences_are_reported() {
        let a = parse_uiautomator_xml(
            r#"<hierarchy><node text="Hello" resource-id="pkg:id/t" class="android.widget.TextView" bounds="[0,0][10,10]"/></hierarchy>"#,
        )
        .unwrap();
        let b = parse_uiautomator_xml(
            r#"<hierarchy><node text="Goodbye" resource-id="pkg:id/t" class="android.widget.TextView" bounds="[40,0][50,10]"/></hierarchy>"#,
        )
        .unwrap();
        let report = compare(&a, &b, CompareOptions { bounds_tolerance: 2, ..CompareOptions::default() });
        assert!(!report.identical);
        assert!(report.differences.iter().any(|d| d.kind == "text_differs"));
        assert!(report.differences.iter().any(|d| d.kind == "bounds_differ"));
    }

    /// An app with no recorded phone capture gets a message that says so and
    /// lists what *is* available, instead of a bare "snapshot is unreadable".
    #[test]
    fn missing_recorded_app_names_the_available_captures() {
        let err = recorded_parity(
            "definitely-not-a-recorded-app",
            None,
            None,
            CompareOptions::default(),
        )
        .expect_err("an unknown app must not compare");
        assert_eq!(err.code, "INVALID_ARGUMENT");
        assert!(
            err.message.contains("no recorded device capture"),
            "{}",
            err.message
        );
        assert!(
            err.message.contains("available recorded apps"),
            "{}",
            err.message
        );
        assert!(err.message.contains("counter"), "{}", err.message);
    }
}
