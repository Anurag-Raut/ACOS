//! Bounded artifacts: keep the evidence, drop the bulk (ARTGC).
//!
//! A real ART session captures ~10 MB per C1 frame. A fifteen-step scenario
//! therefore wrote 0.5–2 GB per run: the mirrored `art-frames/live-frames/`
//! captures plus `frames/` PNGs that no step ever declared, dominated by
//! byte-identical copies. One integration worktree reached 20 GB and the host
//! disk hit 85 % three times in a day.
//!
//! The rule implemented here is deliberately conservative:
//!
//! * a **non-pass** run (`fail`, `timeout`, `error`, `unsupported`, `crash`,
//!   `blocked`) keeps **everything** — it is the diagnosis;
//! * a **pass** keeps everything a verdict, report or step declares, plus the
//!   first and the last frame of the run, and drops only the intermediate
//!   frames and the mirrored raw captures that nothing references;
//! * no retained JSON is ever left pointing at a deleted file: before anything
//!   is removed, every retained JSON document is scanned for the candidate
//!   paths and any hit un-prunes that file. `art-frames/live-session.json` is
//!   rewritten so its `png` fields only name files that still exist.
//!
//! The decision is a pure function of the run status ([`decision`]), so the
//! per-status behaviour is unit-tested without running an app.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use acos_protocol::verdict::Verdict;
use acos_protocol::Status;
use serde::Serialize;

use crate::error::VerifyError;

/// Environment variable that selects the retention mode.
///
/// `acos verify --keep-artifacts <mode>` sets it for the run; the serve/MCP
/// artifact path reads the same variable, so every entry point agrees.
pub const KEEP_ARTIFACTS_ENV: &str = "ACOS_KEEP_ARTIFACTS";

/// What a run leaves behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepArtifacts {
    /// Keep every artifact of every run (the pre-ARTGC behaviour).
    All,
    /// Keep declared/referenced evidence on a pass; keep everything on a
    /// non-pass. This is the default.
    Referenced,
    /// On a pass keep only the reports, trees and logs — no frames at all.
    /// A pass then proves its steps from trees/logs, not from pixels.
    NoneOnPass,
}

impl KeepArtifacts {
    /// Parse the CLI/env spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "all" => Some(KeepArtifacts::All),
            "referenced" => Some(KeepArtifacts::Referenced),
            "none-on-pass" | "none_on_pass" | "none" => Some(KeepArtifacts::NoneOnPass),
            _ => None,
        }
    }

    /// The canonical spelling (`all`, `referenced`, `none-on-pass`).
    pub fn as_str(self) -> &'static str {
        match self {
            KeepArtifacts::All => "all",
            KeepArtifacts::Referenced => "referenced",
            KeepArtifacts::NoneOnPass => "none-on-pass",
        }
    }

    /// Resolve the mode: explicit flag, then `ACOS_KEEP_ARTIFACTS`, then the
    /// default. An unparseable value is reported (never silently ignored) and
    /// falls back to the caller-provided value (the flag) or the default.
    pub fn resolve(flag: Option<&str>) -> (Self, Option<String>) {
        if let Some(value) = flag {
            match Self::parse(value) {
                Some(mode) => return (mode, None),
                None => {
                    return (
                        KeepArtifacts::default(),
                        Some(format!(
                            "--keep-artifacts value {value:?} is not one of all|referenced|none-on-pass"
                        )),
                    )
                }
            }
        }
        match std::env::var(KEEP_ARTIFACTS_ENV) {
            Ok(value) if !value.trim().is_empty() => match Self::parse(&value) {
                Some(mode) => (mode, None),
                None => (
                    KeepArtifacts::default(),
                    Some(format!(
                        "{KEEP_ARTIFACTS_ENV}={value:?} is not one of all|referenced|none-on-pass"
                    )),
                ),
            },
            _ => (KeepArtifacts::default(), None),
        }
    }

    /// The mode the current process runs with.
    pub fn from_env() -> (Self, Option<String>) {
        Self::resolve(None)
    }
}

impl Default for KeepArtifacts {
    fn default() -> Self {
        KeepArtifacts::Referenced
    }
}

impl std::fmt::Display for KeepArtifacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The retention decision for one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Keep the whole artifacts directory.
    KeepAll,
    /// Keep declared evidence plus the first and last frame.
    PruneUnreferenced,
    /// Keep reports/trees/logs only; drop every frame.
    PruneAllFrames,
}

impl Decision {
    /// The canonical spelling used in reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::KeepAll => "keep-all",
            Decision::PruneUnreferenced => "prune-unreferenced",
            Decision::PruneAllFrames => "prune-all-frames",
        }
    }
}

/// Decide what a run of `status` leaves behind under `mode`.
///
/// This is the whole safety rule in one place: only a **pass** is ever pruned,
/// and only under the modes that ask for it.
pub fn decision(status: Status, mode: KeepArtifacts) -> Decision {
    if status != Status::Pass {
        return Decision::KeepAll;
    }
    match mode {
        KeepArtifacts::All => Decision::KeepAll,
        KeepArtifacts::Referenced => Decision::PruneUnreferenced,
        KeepArtifacts::NoneOnPass => Decision::PruneAllFrames,
    }
}

/// One pruned file (relative to the artifacts directory).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrunedFile {
    /// Path relative to the artifacts directory.
    pub path: String,
    /// Size in bytes before removal.
    pub bytes: u64,
}

/// What retention did to one artifacts directory.
#[derive(Debug, Clone, Serialize)]
pub struct RetentionReport {
    /// Selected mode (`all`, `referenced`, `none-on-pass`).
    pub mode: String,
    /// The decision taken.
    pub decision: String,
    /// Human-readable reason (why anything was or was not pruned).
    pub reason: String,
    /// Files kept, and their total size.
    pub kept_files: u64,
    /// Bytes kept.
    pub kept_bytes: u64,
    /// Retained PNG frames that were losslessly re-encoded (same pixels,
    /// stored-deflate "raw buffer" PNG replaced by a real compressed one).
    #[serde(skip_serializing_if = "is_zero")]
    pub frames_reencoded: u64,
    /// Bytes saved by re-encoding retained frames.
    #[serde(skip_serializing_if = "is_zero")]
    pub reencode_saved_bytes: u64,
    /// Files pruned, and the bytes freed.
    pub pruned_files: u64,
    /// Bytes freed by this run.
    pub pruned_bytes: u64,
    /// The pruned files (capped at [`PRUNED_LIST_LIMIT`] entries).
    pub pruned: Vec<PrunedFile>,
    /// True when the pruned list was truncated.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pruned_truncated: bool,
    /// Candidate files that a retained JSON document referenced, so they were
    /// kept. Never empty silently: it names why a file survived.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kept_because_referenced: Vec<String>,
    /// Config problems (an unparseable mode) that fell back to the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// Reported pruned paths are capped so the report stays small.
pub const PRUNED_LIST_LIMIT: usize = 200;

/// `retention.json` — the record of what happened, written next to the verdict.
pub const RETENTION_JSON: &str = "retention.json";

/// Frame directory and mirrored-capture directory names.
const FRAMES_DIR: &str = "frames";
const ART_FRAMES_DIR: &str = "art-frames";
const LIVE_FRAMES_DIR: &str = "live-frames";
const LIVE_SESSION_JSON: &str = "live-session.json";

/// Apply retention to one artifacts directory.
///
/// `verdict` supplies the step evidence and frame sequences; `status` decides
/// whether anything is pruned at all. The report is also written to
/// `retention.json` inside `dir` unless the directory is not writable, in which
/// case the run keeps its artifacts and the report says so.
pub fn apply(
    dir: &Path,
    mode: KeepArtifacts,
    status: Status,
    verdict: &Verdict,
) -> Result<RetentionReport, VerifyError> {
    apply_with_warning(dir, mode, status, verdict, None)
}

/// Apply retention and record a configuration warning (an unparseable
/// `ACOS_KEEP_ARTIFACTS`) in the report instead of dropping it.
pub(crate) fn apply_with_warning(
    dir: &Path,
    mode: KeepArtifacts,
    status: Status,
    verdict: &Verdict,
    warning: Option<String>,
) -> Result<RetentionReport, VerifyError> {
    let decision = decision(status, mode);
    let mut report = RetentionReport {
        mode: mode.as_str().to_string(),
        decision: decision.as_str().to_string(),
        reason: String::new(),
        kept_files: 0,
        kept_bytes: 0,
        pruned_files: 0,
        pruned_bytes: 0,
        pruned: Vec::new(),
        pruned_truncated: false,
        frames_reencoded: 0,
        reencode_saved_bytes: 0,
        kept_because_referenced: Vec::new(),
        warning,
    };

    if decision == Decision::KeepAll {
        report.reason = if status == Status::Pass {
            format!("mode `{mode}` keeps every artifact")
        } else {
            format!(
                "status `{}` is not a pass: a non-pass run keeps all of its artifacts",
                status.as_str()
            )
        };
        let (files, bytes) = dir_usage(dir);
        report.kept_files = files;
        report.kept_bytes = bytes;
        write_report(dir, &report);
        return Ok(report);
    }

    let mut keep: BTreeSet<PathBuf> = BTreeSet::new();
    let mut candidates: Vec<PathBuf> = Vec::new();

    // 1. Everything the verdict declares: step evidence (frames, trees, steps,
    //    logs) and each step's frame sequence, which the validator requires.
    for step in &verdict.steps {
        if let Some(seq) = step.frame_seq {
            keep.insert(PathBuf::from(acos_protocol::artifacts::frame_file_name(
                seq,
            )));
            keep.insert(PathBuf::from(acos_protocol::artifacts::tree_file_name(seq)));
        }
        for evidence in &step.evidence {
            let path = PathBuf::from(&evidence.path);
            if is_safe_relative(&path) {
                keep.insert(path);
            }
        }
    }

    // 2. Every frame the run produced, sorted, so the first and last frame of
    //    the run survive even when no step named them.
    let mut frames = list_sorted(dir.join(FRAMES_DIR));
    if decision == Decision::PruneUnreferenced {
        if let (Some(first), Some(last)) = (frames.first(), frames.last()) {
            keep.insert(PathBuf::from(FRAMES_DIR).join(first));
            keep.insert(PathBuf::from(FRAMES_DIR).join(last));
        }
        candidates.extend(
            frames
                .drain(..)
                .map(|name| PathBuf::from(FRAMES_DIR).join(name)),
        );
    } else {
        // none-on-pass: no frame at all survives, so no first/last exemption.
        candidates.extend(
            frames
                .drain(..)
                .map(|name| PathBuf::from(FRAMES_DIR).join(name)),
        );
    }

    // 3. The mirrored raw captures. Only the first and last capture is kept
    //    (session entry and exit state); the rest are intermediate frames that
    //    duplicate the verifier's `frames/`.
    let mut captures = list_capture_order(dir.join(ART_FRAMES_DIR).join(LIVE_FRAMES_DIR));
    if decision == Decision::PruneUnreferenced {
        if let (Some(first), Some(last)) = (captures.first(), captures.last()) {
            keep.insert(
                PathBuf::from(ART_FRAMES_DIR)
                    .join(LIVE_FRAMES_DIR)
                    .join(first),
            );
            keep.insert(
                PathBuf::from(ART_FRAMES_DIR)
                    .join(LIVE_FRAMES_DIR)
                    .join(last),
            );
        }
    }
    candidates.extend(captures.drain(..).map(|name| {
        PathBuf::from(ART_FRAMES_DIR)
            .join(LIVE_FRAMES_DIR)
            .join(name)
    }));

    // 4. Drop candidates that are already declared as kept.
    let mut prune: Vec<PathBuf> = candidates
        .into_iter()
        .filter(|path| !keep.contains(path))
        .filter(|path| dir.join(path).is_file())
        .collect();
    prune.sort();
    prune.dedup();

    // 5. Never delete a file a retained JSON document names. Scan every JSON
    //    that survives (verdict, steps, trees, engine/live-session records) for
    //    each candidate's path. A hit un-prunes the file: a report may not
    //    dangle. `live-session.json` is handled separately (rewritten), so its
    //    own references do not block the prune.
    let scan_text = collect_retained_json_text(dir, &keep);
    let mut still_prune: Vec<PathBuf> = Vec::new();
    for path in prune {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let rel = path.to_string_lossy().to_string();
        if name.is_empty() {
            continue;
        }
        if scan_text.contains(&name) || scan_text.contains(&rel) {
            report.kept_because_referenced.push(rel);
            keep.insert(path);
        } else {
            still_prune.push(path);
        }
    }
    prune = still_prune;

    // 6. Remove, and account for it.
    let mut pruned: Vec<PrunedFile> = Vec::new();
    for path in &prune {
        let full = dir.join(path);
        let size = std::fs::metadata(&full).map(|meta| meta.len()).unwrap_or(0);
        match std::fs::remove_file(&full) {
            Ok(()) => {
                report.pruned_bytes += size;
                report.pruned_files += 1;
                pruned.push(PrunedFile {
                    path: path.to_string_lossy().to_string(),
                    bytes: size,
                });
            }
            Err(error) => {
                // A file we cannot remove stays: evidence is never lost to a
                // permission problem, and the report says so.
                report
                    .kept_because_referenced
                    .push(format!("{} (kept: {error})", path.to_string_lossy()));
            }
        }
    }

    // 7. Rewrite the session record so no `png` field names a pruned capture.
    let pruned_names: BTreeSet<String> = pruned
        .iter()
        .filter_map(|entry| {
            Path::new(&entry.path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .collect();
    if !pruned_names.is_empty() {
        rewrite_live_session(dir, &pruned_names);
    }

    // 8. Re-encode the retained PNGs. The ART capture path writes PNGs with
    //    zlib *stored* blocks (an unfiltered raw buffer with a PNG wrapper: ~10.4
    //    MB for a 1080x2400 frame), because the encoder is on the screenshot
    //    critical path. Retained evidence is not: it is re-encoded losslessly
    //    (same pixels, real deflate + Sub filtering) so the frames a gate reads
    //    stay byte-exact in content and ~10x smaller on disk.
    let (reencoded, saved) = reencode_retained_frames(dir);
    report.frames_reencoded = reencoded;
    report.reencode_saved_bytes = saved;

    let (files, bytes) = dir_usage(dir);
    report.kept_files = files;
    report.kept_bytes = bytes;
    report.reason = match decision {
        Decision::PruneAllFrames => {
            format!("passing run (mode `{mode}`): reports, trees and logs kept, no frames")
        }
        _ => format!(
            "passing run (mode `{mode}`): declared evidence plus the first and last frame kept, \
             intermediate frames and mirrored captures pruned"
        ),
    };
    if pruned.len() > PRUNED_LIST_LIMIT {
        report.pruned_truncated = true;
        pruned.truncate(PRUNED_LIST_LIMIT);
    }
    report.pruned = pruned;
    report.kept_because_referenced.sort();
    report.kept_because_referenced.dedup();
    write_report(dir, &report);
    Ok(report)
}

/// Strip frame evidence from a verdict before a `none-on-pass` run writes it.
///
/// `none-on-pass` says "no pixels at all". The verdict must then stop declaring
/// frames — a report may never name a file that retention is about to remove —
/// so the declared frame artifacts, each step's `frame_seq`, the named `capture`
/// evidence and the `captures` map are dropped, and the trees, logs and
/// assertion results remain the evidence. Returns the number of declarations
/// removed.
pub fn strip_frame_evidence(verdict: &mut Verdict) -> usize {
    let mut stripped = 0;
    for step in &mut verdict.steps {
        let before = step.evidence.len();
        step.evidence
            .retain(|evidence| evidence.kind != "frame" && evidence.kind != "capture");
        stripped += before - step.evidence.len();
        if step.frame_seq.take().is_some() {
            stripped += 1;
        }
        // A name without its file would break the captures invariant.
        if step.name.take().is_some() {
            stripped += 1;
        }
    }
    stripped += verdict.captures.len();
    verdict.captures.clear();
    stripped
}

/// Reconcile records the engine writes *after* the verifier's retention pass.
///
/// The ART launcher writes `art-frames/live-session.json` (the per-command
/// session transcript) and `art-frames/session-exit.json` at shutdown, which is
/// after the executor pruned the mirrored captures. Those late records still
/// name the captures that were removed, so they are rewritten here: a `png`
/// field whose file no longer exists is replaced by `png_pruned: true`, and the
/// document gains a `retention` block. Idempotent, and safe to call on a
/// non-pass run (nothing was pruned, so nothing changes).
///
/// Returns the number of dangling references removed.
pub fn finalize(dir: &Path) -> u64 {
    let path = dir.join(ART_FRAMES_DIR).join(LIVE_SESSION_JSON);
    let Ok(body) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&body) else {
        return 0;
    };
    let mut stripped = 0_u64;
    if let Some(steps) = doc.get_mut("steps").and_then(|steps| steps.as_array_mut()) {
        for step in steps.iter_mut() {
            let Some(png) = step
                .get("png")
                .and_then(|png| png.as_str())
                .map(str::to_string)
            else {
                continue;
            };
            if resolves_inside(dir, &png) {
                continue;
            }
            if let Some(object) = step.as_object_mut() {
                object.remove("png");
                object.insert("png_pruned".to_string(), serde_json::json!(true));
                stripped += 1;
            }
        }
    }
    if stripped > 0 {
        if let Some(object) = doc.as_object_mut() {
            object.insert(
                "retention".to_string(),
                serde_json::json!({
                    "policy": "referenced",
                    "pruned_captures": stripped,
                    "note": "passing run: intermediate C1 captures were pruned; the frames a step \
                             declared are in frames/<seq>.png",
                }),
            );
        }
        if let Ok(text) = serde_json::to_string_pretty(&doc) {
            let _ = std::fs::write(&path, text);
        }
    }
    stripped
}

/// Does `value` name an existing file inside `dir`?
///
/// Accepts an artifacts-relative path (`frames/1.png`) and a path recorded
/// relative to the caller's cwd (`out/<scenario>/art-frames/...`): the trailing
/// `art-frames/...` or `frames/...` segment is tried against the directory.
fn resolves_inside(dir: &Path, value: &str) -> bool {
    let relative = value.trim_start_matches("./");
    if dir.join(relative).is_file() {
        return true;
    }
    for marker in ["art-frames/", "frames/", "tree/", "steps/", "logs/"] {
        if let Some(index) = value.find(marker) {
            if dir.join(&value[index..]).is_file() {
                return true;
            }
        }
    }
    false
}

/// Losslessly re-encode every PNG retained under `frames/` and
/// `art-frames/live-frames/`. Returns (files re-encoded, bytes saved).
///
/// A re-encode that would not shrink the file, or that fails to decode, leaves
/// the original bytes untouched: evidence is never replaced by a lossy or
/// unreadable variant.
///
/// SHOTZ (ADR-0027 #6) made the capture encoder write real deflate, so a frame
/// this process wrote is already compressed: [`is_stored_zlib_png`] recognises
/// the old stored-block layout and only those files are re-encoded. Re-encoding
/// an already-compressed frame would cost a decode + encode per frame to save
/// nothing (and would rewrite the file the verdict's `md5` names).
pub fn reencode_retained_frames(dir: &Path) -> (u64, u64) {
    let mut count = 0_u64;
    let mut saved = 0_u64;
    for sub in [FRAMES_DIR, "art-frames/live-frames"] {
        for name in list_sorted(dir.join(sub)) {
            if !name.ends_with(".png") {
                continue;
            }
            let path = dir.join(sub).join(&name);
            let Ok(original) = std::fs::read(&path) else {
                continue;
            };
            if !is_stored_zlib_png(&original) {
                // Already deflated by the capture path (or by an earlier
                // re-encode): a no-op, not a second encode.
                continue;
            }
            let Some(smaller) = reencode_png(&original) else {
                continue;
            };
            if smaller.len() >= original.len() {
                continue;
            }
            if std::fs::write(&path, &smaller).is_ok() {
                count += 1;
                saved += (original.len() - smaller.len()) as u64;
            }
        }
    }
    (count, saved)
}

/// Is this PNG's IDAT a zlib stream of **stored** (uncompressed) blocks?
///
/// The pre-SHOTZ capture encoder wrote exactly that: filter 0 per row and stored
/// blocks, so every 1080x2400 frame was 10 371 258 B. Such a file is the only
/// kind worth re-encoding losslessly; a frame the capture path already deflated
/// is left byte-for-byte alone.
///
/// The check is one byte: the first deflate block header's `BTYPE` bits. A file
/// that is not a PNG, has no IDAT, or is an interlaced/odd format answers
/// `false` (leave it alone — the caller's fallback is "do not touch").
pub fn is_stored_zlib_png(bytes: &[u8]) -> bool {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if !bytes.starts_with(&SIGNATURE) {
        return false;
    }
    let mut offset = SIGNATURE.len();
    while offset + 8 <= bytes.len() {
        let length = u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as usize;
        let kind = &bytes[offset + 4..offset + 8];
        let body = offset + 8;
        if kind == b"IDAT" {
            // zlib header (2 bytes: CMF/FLG) then the first deflate block
            // header: BFINAL is bit 0, BTYPE is bits 1-2. BTYPE == 0 is stored.
            if body + 3 > bytes.len() {
                return false;
            }
            let block = bytes[body + 2];
            return block & 0b110 == 0;
        }
        if kind == b"IEND" {
            return false;
        }
        offset = body.saturating_add(length).saturating_add(4);
    }
    false
}

/// Re-encode an 8-bit RGB/RGBA PNG with real deflate and Sub filtering.
///
/// Returns `None` when the image is not one of those formats or the decode
/// fails; callers then keep the original bytes.
fn reencode_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0_u8; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buffer).ok()?;
    let info = frame;
    let color = match info.color_type {
        png::ColorType::Rgba => png::ColorType::Rgba,
        png::ColorType::Rgb => png::ColorType::Rgb,
        _ => return None,
    };
    let pixels = &buffer[..info.buffer_size()];
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, info.width, info.height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Best);
        encoder.set_filter(png::FilterType::Sub);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(pixels).ok()?;
        writer.finish().ok()?;
    }
    Some(out)
}

/// Directory names that hold evidence but are never pruned file-by-file.
fn is_safe_relative(path: &Path) -> bool {
    path.components().all(|component| {
        matches!(
            component,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    })
}

/// File names in `dir`, sorted ascending; empty when the directory is absent.
fn list_sorted(dir: PathBuf) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// Captures in session order.
///
/// The launcher names a capture `…-frame-<n>-g<gen>-c<capture>.png`, where the
/// trailing `c<n>` counts captures in the session. Sorting on that counter puts
/// the session's first and last frame first and last, whatever the layer or
/// generation. Names without the counter fall back to lexical order.
fn list_capture_order(dir: PathBuf) -> Vec<String> {
    let mut names = list_sorted(dir);
    names.sort_by(|a, b| capture_index(a).cmp(&capture_index(b)).then(a.cmp(b)));
    names
}

fn capture_index(name: &str) -> u64 {
    let stem = name
        .rsplit_once(".png")
        .map(|(stem, _)| stem)
        .unwrap_or(name);
    let Some((_, tail)) = stem.rsplit_once("-c") else {
        return u64::MAX;
    };
    tail.parse::<u64>().unwrap_or(u64::MAX)
}

/// Concatenated text of every retained JSON/JSONL document under `dir`
/// (skipping candidates and the rewritten live-session record).
fn collect_retained_json_text(dir: &Path, keep: &BTreeSet<PathBuf>) -> String {
    let mut text = String::new();
    for relative in walk_files(dir) {
        let is_json = relative.extension().is_some_and(|ext| ext == "json")
            || relative.extension().is_some_and(|ext| ext == "jsonl");
        if !is_json {
            continue;
        }
        let is_live_session = relative
            .file_name()
            .is_some_and(|name| name == LIVE_SESSION_JSON);
        if is_live_session {
            continue;
        }
        let _ = keep;
        if let Ok(body) = std::fs::read_to_string(dir.join(&relative)) {
            text.push_str(&body);
            text.push('\n');
        }
    }
    text
}

/// Every regular file under `dir`, relative to it, deepest first is not needed.
fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                // Symlinked directories are not followed (artifact dirs must
                // stay inside the tree).
                if !kind.is_symlink() {
                    stack.push(path);
                }
            } else if kind.is_file() {
                if let Ok(relative) = path.strip_prefix(dir) {
                    found.push(relative.to_path_buf());
                }
            }
        }
    }
    found.sort();
    found
}

/// Rewrite `art-frames/live-session.json` so it never names a pruned capture.
fn rewrite_live_session(dir: &Path, pruned_names: &BTreeSet<String>) {
    let path = dir.join(ART_FRAMES_DIR).join(LIVE_SESSION_JSON);
    let Ok(body) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&body) else {
        return;
    };
    let mut rewritten = 0_u64;
    if let Some(steps) = doc.get_mut("steps").and_then(|steps| steps.as_array_mut()) {
        for step in steps.iter_mut() {
            let Some(png) = step.get("png").and_then(|png| png.as_str()) else {
                continue;
            };
            let name = Path::new(png)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            if pruned_names.contains(&name) {
                let object = step.as_object_mut();
                if let Some(object) = object {
                    object.remove("png");
                    object.insert("png_pruned".to_string(), serde_json::json!(true));
                    rewritten += 1;
                }
            }
        }
    }
    if rewritten == 0 {
        return;
    }
    if let Some(object) = doc.as_object_mut() {
        object.insert(
            "retention".to_string(),
            serde_json::json!({
                "policy": "referenced",
                "pruned_captures": rewritten,
                "note": "passing run: intermediate C1 captures were pruned; the frames a step \
                         declared are in frames/<seq>.png",
            }),
        );
    }
    if let Ok(text) = serde_json::to_string_pretty(&doc) {
        let _ = std::fs::write(&path, text);
    }
}

/// (files, bytes) currently under `dir`.
pub fn dir_usage(dir: &Path) -> (u64, u64) {
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    for relative in walk_files(dir) {
        if let Ok(meta) = std::fs::metadata(dir.join(&relative)) {
            files += 1;
            bytes += meta.len();
        }
    }
    (files, bytes)
}

fn write_report(dir: &Path, report: &RetentionReport) {
    if let Ok(text) = serde_json::to_string_pretty(report) {
        let _ = std::fs::write(dir.join(RETENTION_JSON), text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_protocol::identity::{DisplaySpec, IsolationLevel};
    use acos_protocol::verdict::{
        AppIdentity, ArtifactRef, Reproducer, ScenarioIdentity, StepResult,
    };
    use acos_protocol::Fidelity;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("acos-retention-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, relative: &str, bytes: &[u8]) {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn verdict_with(steps: Vec<(u64, Vec<&str>)>) -> Verdict {
        let mut verdict = Verdict::new(
            Status::Pass,
            Fidelity::NativeRuntime,
            acos_protocol::Backend::AcosNative,
            IsolationLevel::Process,
            "req_retention",
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:01Z",
            AppIdentity {
                path: "a.apk".to_string(),
                package: Some("dev.acos.fixture".to_string()),
                sha256: "apk-sha".to_string(),
            },
            ScenarioIdentity {
                path: "s.yaml".to_string(),
                name: "scenario".to_string(),
                sha256: "scenario-sha".to_string(),
            },
            Reproducer::new(
                "0.1.0",
                "a.apk",
                "apk-sha",
                "s.yaml",
                "scenario-sha",
                IsolationLevel::Process,
                DisplaySpec::offscreen(1080, 2400, 420),
                Fidelity::NativeRuntime,
                acos_protocol::Backend::AcosNative,
            ),
        );
        for (index, (seq, refs)) in steps.into_iter().enumerate() {
            let mut step = StepResult::new(index, "tap", Status::Pass, 1);
            step.frame_seq = Some(seq);
            for reference in &refs {
                step.evidence
                    .push(ArtifactRef::new("frame", (*reference).to_string()));
            }
            verdict.steps.push(step);
        }
        verdict
    }

    #[test]
    fn decision_never_prunes_a_non_pass() {
        for status in [
            Status::Fail,
            Status::Timeout,
            Status::Error,
            Status::Unsupported,
            Status::Crash,
            Status::Blocked,
        ] {
            for mode in [
                KeepArtifacts::All,
                KeepArtifacts::Referenced,
                KeepArtifacts::NoneOnPass,
            ] {
                assert_eq!(
                    decision(status, mode),
                    Decision::KeepAll,
                    "{status:?} under {mode:?} must keep everything"
                );
            }
        }
        assert_eq!(
            decision(Status::Pass, KeepArtifacts::All),
            Decision::KeepAll
        );
        assert_eq!(
            decision(Status::Pass, KeepArtifacts::Referenced),
            Decision::PruneUnreferenced
        );
        assert_eq!(
            decision(Status::Pass, KeepArtifacts::NoneOnPass),
            Decision::PruneAllFrames
        );
    }

    #[test]
    fn mode_parsing_and_default() {
        assert_eq!(KeepArtifacts::parse("ALL"), Some(KeepArtifacts::All));
        assert_eq!(
            KeepArtifacts::parse(" referenced "),
            Some(KeepArtifacts::Referenced)
        );
        assert_eq!(
            KeepArtifacts::parse("none-on-pass"),
            Some(KeepArtifacts::NoneOnPass)
        );
        assert_eq!(KeepArtifacts::parse("keep-everything"), None);
        assert_eq!(KeepArtifacts::default(), KeepArtifacts::Referenced);
        assert_eq!(KeepArtifacts::Referenced.as_str(), "referenced");
    }

    #[test]
    fn passing_run_keeps_referenced_frames_and_first_last_only() {
        let dir = temp_dir("referenced");
        for seq in 1..=5 {
            write(&dir, &format!("frames/{seq}.png"), b"png-bytes");
            write(&dir, &format!("tree/{seq}.json"), b"{\"nodes\":[]}");
        }
        write(&dir, "steps/00-tap.json", b"{\"index\":0}");
        write(&dir, "events.jsonl", b"{}\n");
        let verdict = verdict_with(vec![(1, vec!["frames/1.png"]), (3, vec!["frames/3.png"])]);
        let report = apply(&dir, KeepArtifacts::Referenced, Status::Pass, &verdict).unwrap();
        assert!(
            dir.join("frames/1.png").is_file(),
            "referenced frame 1 kept"
        );
        assert!(
            dir.join("frames/3.png").is_file(),
            "referenced frame 3 kept"
        );
        assert!(!dir.join("frames/2.png").exists(), "intermediate dropped");
        assert!(!dir.join("frames/4.png").exists(), "intermediate dropped");
        assert!(
            dir.join("frames/5.png").is_file(),
            "the last frame of the run is kept"
        );
        assert_eq!(report.pruned_files, 2);
        assert_eq!(report.pruned_bytes, 18);
        assert!(dir.join(RETENTION_JSON).is_file(), "report is written");
        assert!(dir.join("tree/2.json").is_file(), "trees are never pruned");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failing_run_keeps_every_frame_and_capture() {
        let dir = temp_dir("failing");
        for seq in 1..=4 {
            write(&dir, &format!("frames/{seq}.png"), b"png");
        }
        write(
            &dir,
            "art-frames/live-frames/acos-c1-layer6-frame-1-g1-c1.png",
            b"cap1",
        );
        write(
            &dir,
            "art-frames/live-frames/acos-c1-layer6-frame-2-g2-c2.png",
            b"cap2",
        );
        let verdict = verdict_with(vec![(1, vec!["frames/1.png"])]);
        let report = apply(&dir, KeepArtifacts::Referenced, Status::Fail, &verdict).unwrap();
        assert_eq!(report.decision, "keep-all");
        assert_eq!(report.pruned_files, 0);
        assert!(dir.join("frames/4.png").is_file());
        assert!(dir
            .join("art-frames/live-frames/acos-c1-layer6-frame-2-g2-c2.png")
            .is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn live_session_is_rewritten_so_no_png_path_dangles() {
        let dir = temp_dir("live-session");
        for seq in 1..=4 {
            write(&dir, &format!("frames/{seq}.png"), b"png");
        }
        let captures = [
            "acos-c1-layer6-frame-1-g1-c1.png",
            "acos-c1-layer6-frame-2-g2-c2.png",
            "acos-c1-layer6-frame-3-g3-c3.png",
        ];
        for name in captures {
            write(&dir, &format!("art-frames/live-frames/{name}"), b"cap");
        }
        let session = serde_json::json!({
            "steps": [
                {"cmd": "ready"},
                {"cmd": "tree", "png": format!("out/s/art-frames/live-frames/{}", captures[0])},
                {"cmd": "tap", "png": format!("out/s/art-frames/live-frames/{}", captures[1])},
                {"cmd": "tap", "png": format!("out/s/art-frames/live-frames/{}", captures[2])},
            ]
        });
        write(
            &dir,
            "art-frames/live-session.json",
            serde_json::to_string_pretty(&session).unwrap().as_bytes(),
        );
        let verdict = verdict_with(vec![(2, vec!["frames/2.png"])]);
        let report = apply(&dir, KeepArtifacts::Referenced, Status::Pass, &verdict).unwrap();
        assert!(report.pruned_files >= 1);
        let body = std::fs::read_to_string(dir.join("art-frames/live-session.json")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
        for step in doc["steps"].as_array().unwrap() {
            if let Some(png) = step.get("png").and_then(|png| png.as_str()) {
                let name = Path::new(png)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                assert!(
                    dir.join("art-frames/live-frames").join(&name).is_file(),
                    "live-session.json must not name a pruned capture: {name}"
                );
            }
        }
        assert_eq!(doc["retention"]["pruned_captures"], 1);
        // First and last capture survive.
        assert!(dir
            .join("art-frames/live-frames/acos-c1-layer6-frame-1-g1-c1.png")
            .is_file());
        assert!(dir
            .join("art-frames/live-frames/acos-c1-layer6-frame-3-g3-c3.png")
            .is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_retained_json_reference_unprunes_a_candidate() {
        let dir = temp_dir("reference-wins");
        for seq in 1..=4 {
            write(&dir, &format!("frames/{seq}.png"), b"png");
        }
        // A report outside the verdict names frame 2; deleting it would dangle.
        write(
            &dir,
            "art-frames/session-exit.json",
            b"{\"native_crash_path\":\"frames/2.png\"}",
        );
        let verdict = verdict_with(vec![(1, vec!["frames/1.png"])]);
        let report = apply(&dir, KeepArtifacts::Referenced, Status::Pass, &verdict).unwrap();
        assert!(
            dir.join("frames/2.png").is_file(),
            "a referenced candidate must survive"
        );
        assert!(report
            .kept_because_referenced
            .iter()
            .any(|path| path == "frames/2.png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn none_on_pass_drops_every_frame_but_keeps_trees_and_reports() {
        let dir = temp_dir("none-on-pass");
        for seq in 1..=3 {
            write(&dir, &format!("frames/{seq}.png"), b"png");
            write(&dir, &format!("tree/{seq}.json"), b"{}");
        }
        write(&dir, "logs/logcat.txt", b"log");
        let mut verdict = verdict_with(vec![(2, vec!["frames/2.png"])]);
        assert_eq!(strip_frame_evidence(&mut verdict), 2);
        write(
            &dir,
            "verdict.json",
            serde_json::to_vec(&verdict).unwrap().as_slice(),
        );
        let report = apply(&dir, KeepArtifacts::NoneOnPass, Status::Pass, &verdict).unwrap();
        assert_eq!(report.decision, "prune-all-frames");
        for seq in 1..=3 {
            assert!(
                !dir.join(format!("frames/{seq}.png")).exists(),
                "none-on-pass drops frame {seq}"
            );
            assert!(dir.join(format!("tree/{seq}.json")).is_file());
        }
        assert!(dir.join("verdict.json").is_file());
        assert!(dir.join("logs/logcat.txt").is_file());
        // The written verdict no longer declares the removed frames.
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("verdict.json")).unwrap())
                .unwrap();
        assert!(doc["steps"][0].get("frame_seq").is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_declared_frame_survives_even_none_on_pass() {
        // The last-resort rule: a report that still declares a frame wins over
        // the mode. Nothing dangles; the pass simply keeps that one frame.
        let dir = temp_dir("none-on-pass-declared");
        for seq in 1..=3 {
            write(&dir, &format!("frames/{seq}.png"), b"png");
        }
        let verdict = verdict_with(vec![(2, vec!["frames/2.png"])]);
        let report = apply(&dir, KeepArtifacts::NoneOnPass, Status::Pass, &verdict).unwrap();
        assert!(dir.join("frames/2.png").is_file());
        assert!(
            !report
                .pruned
                .iter()
                .any(|entry| entry.path == "frames/2.png"),
            "a declared frame is never in the pruned list"
        );
        assert!(!dir.join("frames/1.png").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A PNG in the ART capture shape *before* SHOTZ: filter 0 rows and zlib
    /// **stored** blocks — the ~10 MB "raw buffer in a PNG wrapper" the launcher
    /// wrote on the capture path. This is the only layout the retention pass
    /// re-encodes, so the helper builds it by hand (the `png` crate cannot write
    /// stored blocks through its public API).
    fn stored_png(width: u32, height: u32) -> Vec<u8> {
        let mut raw = Vec::new();
        for y in 0..height {
            raw.push(0u8); // filter 0 per row
            for x in 0..width {
                let shade = ((x / 8 + y / 8) % 2) as u8;
                raw.extend_from_slice(&[shade * 200, shade * 100, shade * 40, 255]);
            }
        }
        let mut zlib = vec![0x78, 0x01];
        let mut offset = 0usize;
        while offset < raw.len() {
            let len = (raw.len() - offset).min(65_535);
            let last = offset + len == raw.len();
            zlib.push(u8::from(last));
            zlib.extend_from_slice(&(len as u16).to_le_bytes());
            zlib.extend_from_slice(&(!(len as u16)).to_le_bytes());
            zlib.extend_from_slice(&raw[offset..offset + len]);
            offset += len;
        }
        let mut a = 1u32;
        let mut b = 0u32;
        for &byte in &raw {
            a = (a + u32::from(byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());

        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        test_chunk(&mut out, b"IHDR", &ihdr);
        test_chunk(&mut out, b"IDAT", &zlib);
        test_chunk(&mut out, b"IEND", &[]);
        out
    }

    /// A PNG the SHOTZ capture encoder writes: real deflate, filtered rows.
    fn deflated_png(width: u32, height: u32) -> Vec<u8> {
        let mut raw = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let shade = ((x / 8 + y / 8) % 2) as u8;
                raw.extend_from_slice(&[shade * 200, shade * 100, shade * 40, 255]);
            }
        }
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&raw).unwrap();
            writer.finish().unwrap();
        }
        out
    }

    fn test_crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    fn test_chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let mut crc_input = Vec::with_capacity(4 + body.len());
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(body);
        out.extend_from_slice(&test_crc32(&crc_input).to_be_bytes());
    }

    fn decode_pixels(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buffer = vec![0_u8; reader.output_buffer_size()];
        let frame = reader.next_frame(&mut buffer).unwrap();
        let (width, height) = (frame.width, frame.height);
        buffer.truncate(frame.buffer_size());
        (width, height, buffer)
    }

    #[test]
    fn reencoding_keeps_pixels_and_shrinks_the_frame() {
        let dir = temp_dir("reencode");
        let original = stored_png(240, 320);
        write(&dir, "frames/1.png", &original);
        let (count, saved) = reencode_retained_frames(&dir);
        assert_eq!(count, 1);
        let shrunk = std::fs::read(dir.join("frames/1.png")).unwrap();
        assert!(saved > 0);
        assert!(
            shrunk.len() < original.len(),
            "re-encoded frame must be smaller: {} vs {}",
            shrunk.len(),
            original.len()
        );
        let (ow, oh, opixels) = decode_pixels(&original);
        let (nw, nh, npixels) = decode_pixels(&shrunk);
        assert_eq!((ow, oh), (nw, nh));
        assert_eq!(opixels, npixels, "re-encoding is lossless");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_already_compressed_frame_is_not_reencoded() {
        // SHOTZ: the capture path now writes real deflate. Re-encoding that
        // would be a decode + encode per frame to save nothing, and it would
        // rewrite the file the verdict's md5 names.
        let dir = temp_dir("reencode-skip");
        let deflated = deflated_png(240, 320);
        write(&dir, "frames/1.png", &deflated);
        let (count, saved) = reencode_retained_frames(&dir);
        assert_eq!(count, 0, "an already-compressed frame must be skipped");
        assert_eq!(saved, 0);
        assert_eq!(
            std::fs::read(dir.join("frames/1.png")).unwrap(),
            deflated,
            "the frame's bytes must be untouched"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_stored_block_detector_separates_the_two_layouts() {
        assert!(is_stored_zlib_png(&stored_png(16, 8)));
        assert!(!is_stored_zlib_png(&deflated_png(16, 8)));
        // Not a PNG, an empty file, a PNG with no IDAT.
        assert!(!is_stored_zlib_png(b""));
        assert!(!is_stored_zlib_png(b"GIF89a"));
        let mut header_only = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        test_chunk(&mut header_only, b"IHDR", &[0; 13]);
        test_chunk(&mut header_only, b"IEND", &[]);
        assert!(!is_stored_zlib_png(&header_only));
    }

    #[test]
    fn capture_order_puts_first_and_last_first() {
        let names = [
            "acos-c1-layer7-frame-1-g231-c7.png",
            "acos-c1-layer6-frame-193-g196-c4.png",
            "acos-c1-layer6-frame-83-g86-c1.png",
            "acos-c1-layer7-frame-72-g302-c10.png",
        ];
        let mut sorted: Vec<&str> = names.to_vec();
        sorted.sort_by(|a, b| capture_index(a).cmp(&capture_index(b)).then(a.cmp(b)));
        assert_eq!(sorted[0], names[2]);
        assert_eq!(sorted[3], names[3]);
    }
}
