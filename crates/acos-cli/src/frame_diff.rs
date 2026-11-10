//! `acos frame-diff <reference> <candidate>` — numeric frame comparison.
//!
//! Loads two images (any of the formats `acos-render` decodes: PNG/JPEG/WebP),
//! runs the per-pixel + structural diff in [`acos_render::diff`], and prints the
//! real numbers. It is a measurement command: without explicit thresholds it
//! always exits 0, so a large diff is reported rather than hidden. Pass
//! `--fail-over-mean` / `--fail-under-ssim` to turn it into a gate.
//!
//! It is deliberately strict about dimensions: two frames of different sizes are
//! a structured `INVALID_ARGUMENT`, never a silent crop/resize.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use acos_protocol::{
    Backend as ProtocolBackend, Envelope, ErrorCode, Fidelity, Metrics, ProtocolError,
};
use acos_render::diff::{self, DiffOptions};
use acos_render::image;
use serde_json::{json, Value};

use crate::new_request_id;

/// Arguments for `acos frame-diff`.
#[derive(Debug, clap::Args)]
pub struct FrameDiffArgs {
    /// Reference image (the `art-reference` capture).
    pub reference: PathBuf,

    /// Candidate image (the ACOS-rendered frame).
    pub candidate: PathBuf,

    /// A pixel counts as differing when its largest RGB delta exceeds this.
    #[arg(long, default_value_t = 2)]
    pub threshold: u8,

    /// SSIM Gaussian window side (forced odd, minimum 3).
    #[arg(long = "ssim-window", default_value_t = 11)]
    pub ssim_window: u32,

    /// Coarse structural block side in pixels.
    #[arg(long, default_value_t = 32)]
    pub block: u32,

    /// Exclude an `x0,y0,x1,y1` rectangle (exclusive) from every metric.
    /// Repeatable; use for the Android status bar / navigation bar bands the
    /// ACOS window does not own.
    #[arg(long = "mask", value_parser = parse_mask, action = clap::ArgAction::Append)]
    pub mask: Vec<[u32; 4]>,

    /// Write an RGBA heat map PNG of the differences.
    #[arg(long)]
    pub heatmap: Option<PathBuf>,

    /// Exit non-zero when the overall RGB mean absolute error exceeds this.
    #[arg(long = "fail-over-mean")]
    pub fail_over_mean: Option<f64>,

    /// Exit non-zero when the mean windowed SSIM falls below this.
    #[arg(long = "fail-under-ssim")]
    pub fail_under_ssim: Option<f64>,

    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Run `acos frame-diff`.
pub fn run(args: FrameDiffArgs) -> ExitCode {
    let request_id = new_request_id();
    let started = Instant::now();

    let reference = match load(&args.reference) {
        Ok(img) => img,
        Err((code, msg)) => {
            return fail(&request_id, code, msg, started, args.json);
        }
    };
    let candidate = match load(&args.candidate) {
        Ok(img) => img,
        Err((code, msg)) => {
            return fail(&request_id, code, msg, started, args.json);
        }
    };

    let opts = DiffOptions {
        pixel_threshold: args.threshold,
        ssim_window: args.ssim_window,
        block_size: args.block,
        mask: args.mask.clone(),
    };
    let report = match diff::diff(&reference, &candidate, &opts) {
        Ok(r) => r,
        Err(e) => {
            return fail(
                &request_id,
                ErrorCode::RenderInvalidArgument,
                e.to_string(),
                started,
                args.json,
            );
        }
    };

    let heatmap_path = match &args.heatmap {
        Some(path) => {
            match diff::heatmap_png(&reference, &candidate, args.threshold) {
                Ok(png) => {
                    if let Err(e) = std::fs::write(path, &png) {
                        return fail(
                            &request_id,
                            ErrorCode::Internal,
                            format!("writing heatmap {}: {e}", path.display()),
                            started,
                            args.json,
                        );
                    }
                    Some(path.display().to_string())
                }
                Err(e) => {
                    return fail(
                        &request_id,
                        ErrorCode::RenderInvalidArgument,
                        e.to_string(),
                        started,
                        args.json,
                    );
                }
            }
        }
        None => None,
    };

    let result = json!({
        "reference": args.reference.display().to_string(),
        "candidate": args.candidate.display().to_string(),
        "heatmap": heatmap_path,
        "diff": report,
    });

    let metrics = Metrics::wall_ms(started.elapsed().as_millis() as u64);

    let mean_over = args
        .fail_over_mean
        .filter(|limit| report.mean_abs_error > *limit);
    let ssim_under = args
        .fail_under_ssim
        .filter(|limit| report.mean_ssim < *limit);

    if mean_over.is_some() || ssim_under.is_some() {
        let details = json!({
            "mean_abs_error": report.mean_abs_error,
            "mean_ssim": report.mean_ssim,
            "fail_over_mean": args.fail_over_mean,
            "fail_under_ssim": args.fail_under_ssim,
        });
        let msg = match (mean_over, ssim_under) {
            (Some(limit), _) => format!(
                "mean absolute error {:.3} exceeds limit {:.3}",
                report.mean_abs_error, limit
            ),
            (_, Some(limit)) => format!(
                "mean SSIM {:.4} is below limit {:.4}",
                report.mean_ssim, limit
            ),
            (None, None) => unreachable!(),
        };
        let base = Envelope::failure(
            request_id.clone(),
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            ProtocolError::new(ErrorCode::AssertionFailed, msg).with_details(details),
            metrics,
        );
        let envelope = attach(base, result, &request_id, metrics);
        emit(&envelope, &args, &report);
        return ExitCode::FAILURE;
    }

    let base = Envelope::pass(
        request_id.clone(),
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        metrics,
    );
    let envelope = attach(base, result, &request_id, metrics);
    emit(&envelope, &args, &report);
    ExitCode::SUCCESS
}

/// Attach the result value, falling back to a structured internal error if it
/// cannot be serialized (it always can, but never panic on the agent path).
fn attach(
    base: Envelope,
    result: Value,
    request_id: &str,
    metrics: Metrics,
) -> Envelope {
    match base.with_result(result) {
        Ok(envelope) => envelope,
        Err(_) => Envelope::failure(
            request_id.to_string(),
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            ProtocolError::new(ErrorCode::Internal, "frame-diff result serialization"),
            metrics,
        ),
    }
}

/// Parse `--mask x0,y0,x1,y1` (exclusive rectangle).
fn parse_mask(s: &str) -> Result<[u32; 4], String> {
    let fields: Vec<&str> = s.split(',').collect();
    if fields.len() != 4 {
        return Err(format!("mask must be x0,y0,x1,y1 (got {s:?})"));
    }
    let mut rect = [0u32; 4];
    for (i, field) in fields.iter().enumerate() {
        rect[i] = field
            .trim()
            .parse()
            .map_err(|e| format!("mask field {} of {s:?}: {e}", i))?;
    }
    if rect[0] >= rect[2] || rect[1] >= rect[3] {
        return Err(format!("mask rectangle {s:?} is empty or inverted"));
    }
    Ok(rect)
}

fn load(path: &PathBuf) -> Result<image::DecodedImage, (ErrorCode, String)> {
    let bytes = std::fs::read(path)
        .map_err(|e| (ErrorCode::Internal, format!("reading {}: {e}", path.display())))?;
    image::decode(&bytes)
        .map_err(|e| (ErrorCode::RenderInvalidArgument, format!("{}: {e}", path.display())))
}

fn fail(
    request_id: &str,
    code: ErrorCode,
    message: String,
    started: Instant,
    json_out: bool,
) -> ExitCode {
    let envelope = Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(code, message),
        Metrics::wall_ms(started.elapsed().as_millis() as u64),
    );
    if json_out {
        crate::print_json(&envelope);
    } else if let Some(err) = envelope.error() {
        eprintln!("frame-diff: {}: {}", err.code, err.message);
    }
    ExitCode::FAILURE
}

fn emit(envelope: &Envelope, args: &FrameDiffArgs, report: &diff::DiffReport) {
    if args.json {
        crate::print_json(envelope);
    } else {
        print_text(report);
    }
    if let Some(err) = envelope.error() {
        eprintln!("frame-diff: {}: {}", err.code, err.message);
    }
}

fn print_text(r: &diff::DiffReport) {
    println!(
        "frame-diff {}x{}: {}",
        r.width,
        r.height,
        if r.identical { "IDENTICAL" } else { "DIFFERENT" }
    );
    println!(
        "  per-pixel   mean={:.4} rmse={:.4} max={} psnr={}",
        r.mean_abs_error,
        r.rmse,
        r.max_abs_error,
        r.psnr_db
            .map(|p| format!("{p:.2} dB"))
            .unwrap_or_else(|| "inf".to_string()),
    );
    println!(
        "  channels    R mean={:.4} rmse={:.4} max={} | G mean={:.4} rmse={:.4} max={} | B mean={:.4} rmse={:.4} max={} | A mean={:.4}",
        r.red.mean_abs_error,
        r.red.rmse,
        r.red.max_abs_error,
        r.green.mean_abs_error,
        r.green.rmse,
        r.green.max_abs_error,
        r.blue.mean_abs_error,
        r.blue.rmse,
        r.blue.max_abs_error,
        r.alpha.mean_abs_error,
    );
    println!(
        "  differing   {}/{} ({:.3}%) threshold>{} bbox={}",
        r.differing_pixels,
        r.compared_pixels,
        r.differing_fraction * 100.0,
        r.pixel_threshold,
        r.changed_bbox
            .map(|b| format!("[{},{},{},{}]", b[0], b[1], b[2], b[3]))
            .unwrap_or_else(|| "-".to_string()),
    );
    if !r.mask.is_empty() {
        let rects: Vec<String> = r
            .mask
            .iter()
            .map(|m| format!("[{},{},{},{}]", m[0], m[1], m[2], m[3]))
            .collect();
        println!(
            "  masked      {}/{} pixels ({}); excluded {}",
            r.masked_pixels,
            r.pixel_count,
            r.masked_pixels as f64 / r.pixel_count as f64 * 100.0,
            rects.join(" "),
        );
    }
    println!(
        "  structural  SSIM mean={:.4} min={:.4} global={:.4} (window {}); blocks {}/{} changed ({:.2}%), worst block MAE={:.3}",
        r.mean_ssim,
        r.min_ssim,
        r.global_ssim,
        r.ssim_window,
        r.blocks.changed_blocks,
        r.blocks.blocks_x as u64 * r.blocks.blocks_y as u64,
        r.blocks.changed_fraction * 100.0,
        r.blocks.max_block_mean_abs_error,
    );
}
