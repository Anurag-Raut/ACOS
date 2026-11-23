//! `multiwindow` — real multi-window task geometry and configuration transitions
//! (ADR-0029 module; `docs/MULTIWINDOW_PLAN.md` slice 1).
//!
//! # Profiles
//!
//! * `off` (default: detached) keeps `WINDOWING_MODE_FULLSCREEN` on every task
//!   and refuses every `multiwindow` control op (no fake split).
//! * `real` mutates the session's live [`crate::wms::WmsState`] task bounds and
//!   enqueues a [`crate::host::GuestCommand::Window`] on [`ServiceHost`]'s
//!   guest-command outbox (ADR-0029, 2026-11-18 amendment — option A). The
//!   launcher loop drains the outbox, writes `WINDOW ...` to the owning guest
//!   process, waits for a fresh probe sample, and returns the settled UI tree in
//!   the same control reply.
//!
//! There is **no `stub`/`scripted` profile**: a scripted split would be a fake
//! success, which ADR-0029 item 3 forbids.

use acos_wm::{
    RectI, SplitPosition, TaskRecord, DEFAULT_TASK_ID, WINDOWING_MODE_FULLSCREEN,
    WINDOWING_MODE_MULTI_WINDOW,
};
use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::host::{GuestCommand, ServiceHost};
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass,
};

/// The module id (`acos_launch {"services": [{"id": "multiwindow"}]}`).
pub const MODULE_ID: &str = "multiwindow";

/// Minimum allowed split ratio (fraction of the split axis).
pub const MIN_SPLIT_RATIO: f64 = 0.2;
/// Maximum allowed split ratio (fraction of the split axis).
pub const MAX_SPLIT_RATIO: f64 = 0.8;

const CONTROL_OPS: [ControlOp; 4] = [
    ControlOp {
        name: "enter_split",
        args: "{position?: \"top\"|\"bottom\"|\"left\"|\"right\" (default \"top\"), ratio?: 0.2..=0.8 (default 0.5), task?: i32} — put the foreground task in multi-window bounds and push the activity override Configuration to the owning guest (`bottom` two-activity split is refused until slice 2)",
    },
    ControlOp {
        name: "resize",
        args: "{ratio: 0.2..=0.8, position?: \"top\"|\"bottom\"|\"left\"|\"right\", task?: i32} — resize the active split pane and deliver the new activity override Configuration",
    },
    ControlOp {
        name: "exit",
        args: "{task?: i32} (or empty) — clear task bounds, restore WINDOWING_MODE_FULLSCREEN and deliver the full-screen activity override Configuration",
    },
    ControlOp {
        name: "status",
        args: "none — current multi-window state, active task bounds and transition count",
    },
];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "real multi-window task bounds and windowing-mode transitions on the live WMS \
                  (slice 1: single-task split enter_split/resize/exit via the ServiceHost \
                  guest-command outbox; `off` keeps fullscreen and refuses control ops, no \
                  stub/scripted profile)",
        services: &[],
        replaces: &[],
        process_initializers: &[],
        features: &[],
        providers: &[],
        broadcasts: &[],
        windows: &["TYPE_APPLICATION"],
        system_apps: &[],
        requires: &[],
        requires_host_tools: &[],
        // `stub`/`scripted` are deliberately absent (a fake split would be a fake success).
        profiles: &[Profile::Real],
        budget: Budget {
            ram_mib: 1,
            startup_ms: 5,
        },
        attach: AttachMode::Hot,
        controls: &CONTROL_OPS,
        security: SecurityClass::InProcess,
    };
    &MANIFEST
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ActiveSplit {
    task_id: i32,
    position: SplitPosition,
    ratio: f32,
    record: TaskRecord,
}

/// The `multiwindow` module.
#[derive(Debug)]
pub struct MultiwindowModule {
    profile: Profile,
    active: Option<ActiveSplit>,
    transitions: u64,
}

impl MultiwindowModule {
    /// A detached module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: Profile::Off,
            active: None,
            transitions: 0,
        }
    }

    /// The manifest (registry and tests).
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        manifest_ref()
    }
}

impl Default for MultiwindowModule {
    fn default() -> Self {
        Self::new()
    }
}

fn rect_json(rect: RectI) -> Value {
    json!([rect.left, rect.top, rect.right, rect.bottom])
}

fn parse_ratio(args: &Value, default: Option<f64>, method: &str) -> Result<f32> {
    let raw = match args.get("ratio") {
        None | Some(Value::Null) => match default {
            Some(d) => d,
            None => {
                return Err(ServiceError::BadRequest {
                    interface: MODULE_ID.to_string(),
                    method: method.to_string(),
                    detail: "missing required numeric `ratio` (0.2..=0.8)".to_string(),
                });
            }
        },
        Some(value) => value.as_f64().ok_or_else(|| ServiceError::BadRequest {
            interface: MODULE_ID.to_string(),
            method: method.to_string(),
            detail: format!("`ratio` must be a number in {MIN_SPLIT_RATIO}..={MAX_SPLIT_RATIO}, got {value}"),
        })?,
    };
    if !raw.is_finite() || !(MIN_SPLIT_RATIO..=MAX_SPLIT_RATIO).contains(&raw) {
        return Err(ServiceError::BadRequest {
            interface: MODULE_ID.to_string(),
            method: method.to_string(),
            detail: format!(
                "`ratio` {raw} is out of range ({MIN_SPLIT_RATIO}..={MAX_SPLIT_RATIO})"
            ),
        });
    }
    Ok(raw as f32)
}

fn parse_position(args: &Value, default: SplitPosition, method: &str) -> Result<SplitPosition> {
    match args.get("position") {
        None | Some(Value::Null) => Ok(default),
        Some(value) => {
            let text = value.as_str().ok_or_else(|| ServiceError::BadRequest {
                interface: MODULE_ID.to_string(),
                method: method.to_string(),
                detail: format!(
                    "`position` must be one of top, bottom, left, right; got {value}"
                ),
            })?;
            SplitPosition::parse(text).ok_or_else(|| ServiceError::BadRequest {
                interface: MODULE_ID.to_string(),
                method: method.to_string(),
                detail: format!(
                    "unknown `position` {text:?}; expected top, bottom, left or right"
                ),
            })
        }
    }
}

fn parse_task_id(args: &Value, default: i32, method: &str) -> Result<i32> {
    match args.get("task") {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| ServiceError::BadRequest {
                interface: MODULE_ID.to_string(),
                method: method.to_string(),
                detail: format!("`task` must be a 32-bit integer, got {value}"),
            }),
    }
}

/// Resolve the guest process tag that currently owns the foreground task
/// (`0` = primary process).
fn resolve_process_tag(host: &ServiceHost) -> u32 {
    host.atms()
        .and_then(|atms| {
            let guard = atms
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .top_process
                .as_ref()
                .map(|top| top.load(std::sync::atomic::Ordering::Relaxed))
        })
        .unwrap_or(0)
}

fn no_wms_error(op: &str) -> ServiceError {
    ServiceError::UnsupportedAndroidApi {
        interface: MODULE_ID.to_string(),
        code: 0,
        method: format!(
            "{op}: requires a live WMS (ART engine); this host has no window manager"
        ),
    }
}

impl Module for MultiwindowModule {
    fn manifest(&self) -> &'static ModuleManifest {
        manifest_ref()
    }

    fn attach(
        &mut self,
        _host: &mut ServiceHost,
        profile: Profile,
        _config: &ModuleConfig,
    ) -> Result<()> {
        if !manifest_ref().supports(profile) {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "profile {profile} is not implemented: this module has `off` (fullscreen \
                     only, controls refused) and `real` (live WMS task bounds + guest override \
                     Configuration). A stub/scripted split would be a fake success, so there is none"
                ),
            });
        }
        self.profile = profile;
        self.active = None;
        Ok(())
    }

    fn detach(&mut self, host: &mut ServiceHost) -> Result<()> {
        if let Some(active) = self.active.take() {
            if let Some(wms) = host.wms() {
                let display = {
                    let mut guard = wms
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    guard.clear_all_task_bounds();
                    guard.display()
                };
                let process = resolve_process_tag(host);
                host.push_guest_command(GuestCommand::Window {
                    task: active.task_id,
                    process,
                    rect: display.frame,
                    app_bounds: display.window_frame(),
                    windowing_mode: WINDOWING_MODE_FULLSCREEN,
                });
            }
        }
        self.profile = Profile::Off;
        Ok(())
    }

    fn control(&mut self, host: &mut ServiceHost, op: &str, args: &Value) -> Result<Value> {
        if self.profile != Profile::Real {
            return Err(ServiceError::ModuleUnavailable {
                service: MODULE_ID.to_string(),
                interface: MODULE_ID.to_string(),
                module: MODULE_ID,
                attach: format!(
                    "acos_launch {{\"services\": [{{\"id\": \"{MODULE_ID}\", \"profile\": \"real\"}}]}}"
                ),
            });
        }
        match op {
            "enter_split" => {
                if args.get("bottom").is_some_and(|v| !v.is_null()) {
                    return Err(ServiceError::UnsupportedAndroidApi {
                        interface: MODULE_ID.to_string(),
                        code: 0,
                        method: "enter_split(bottom): two-activity split requires multiwindow \
                                 slice 2 (multi-resume + second task); slice 1 supports \
                                 single-task split with {position, ratio}"
                            .to_string(),
                    });
                }
                let position = parse_position(args, SplitPosition::Top, "enter_split")?;
                let ratio = parse_ratio(args, Some(0.5), "enter_split")?;
                let task_id = parse_task_id(args, DEFAULT_TASK_ID, "enter_split")?;
                let Some(wms) = host.wms() else {
                    return Err(no_wms_error("enter_split"));
                };
                let (record, width_dp, height_dp) = {
                    let mut guard = wms
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let display = guard.display();
                    let rect = display.split_rect(position, ratio);
                    let record =
                        guard.set_task_bounds(task_id, rect, WINDOWING_MODE_MULTI_WINDOW);
                    let w_dp = display.px_to_dp(record.app_bounds.width());
                    let h_dp = display.px_to_dp(record.app_bounds.height());
                    (record, w_dp, h_dp)
                };
                let process = resolve_process_tag(host);
                host.push_guest_command(GuestCommand::Window {
                    task: task_id,
                    process,
                    rect: record.bounds,
                    app_bounds: record.app_bounds,
                    windowing_mode: record.windowing_mode,
                });
                self.active = Some(ActiveSplit {
                    task_id,
                    position,
                    ratio,
                    record,
                });
                self.transitions += 1;
                Ok(json!({
                    "task_id": task_id,
                    "process": process,
                    "position": position.as_str(),
                    "ratio": ratio,
                    "windowing_mode": record.windowing_mode,
                    "in_multi_window_mode": record.is_multi_window(),
                    "bounds": rect_json(record.bounds),
                    "app_bounds": rect_json(record.app_bounds),
                    "width_dp": width_dp,
                    "height_dp": height_dp,
                    "transitions": self.transitions,
                }))
            }
            "resize" => {
                let Some(current) = self.active else {
                    return Err(ServiceError::IllegalState {
                        component: MODULE_ID,
                        detail: "multiwindow is not active; call `enter_split` before `resize`"
                            .to_string(),
                    });
                };
                let ratio = parse_ratio(args, None, "resize")?;
                let position = parse_position(args, current.position, "resize")?;
                let task_id = parse_task_id(args, current.task_id, "resize")?;
                let Some(wms) = host.wms() else {
                    return Err(no_wms_error("resize"));
                };
                let (record, width_dp, height_dp) = {
                    let mut guard = wms
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let display = guard.display();
                    let rect = display.split_rect(position, ratio);
                    let record =
                        guard.set_task_bounds(task_id, rect, WINDOWING_MODE_MULTI_WINDOW);
                    let w_dp = display.px_to_dp(record.app_bounds.width());
                    let h_dp = display.px_to_dp(record.app_bounds.height());
                    (record, w_dp, h_dp)
                };
                let process = resolve_process_tag(host);
                host.push_guest_command(GuestCommand::Window {
                    task: task_id,
                    process,
                    rect: record.bounds,
                    app_bounds: record.app_bounds,
                    windowing_mode: record.windowing_mode,
                });
                self.active = Some(ActiveSplit {
                    task_id,
                    position,
                    ratio,
                    record,
                });
                self.transitions += 1;
                Ok(json!({
                    "task_id": task_id,
                    "process": process,
                    "position": position.as_str(),
                    "ratio": ratio,
                    "windowing_mode": record.windowing_mode,
                    "in_multi_window_mode": record.is_multi_window(),
                    "bounds": rect_json(record.bounds),
                    "app_bounds": rect_json(record.app_bounds),
                    "width_dp": width_dp,
                    "height_dp": height_dp,
                    "transitions": self.transitions,
                }))
            }
            "exit" => {
                let default_task = self.active.map(|a| a.task_id).unwrap_or(DEFAULT_TASK_ID);
                let task_id = parse_task_id(args, default_task, "exit")?;
                let Some(wms) = host.wms() else {
                    return Err(no_wms_error("exit"));
                };
                let (display, width_dp, height_dp) = {
                    let mut guard = wms
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    guard.clear_task_bounds(task_id);
                    let display = guard.display();
                    let w_dp = display.px_to_dp(display.window_frame().width());
                    let h_dp = display.px_to_dp(display.window_frame().height());
                    (display, w_dp, h_dp)
                };
                let process = resolve_process_tag(host);
                let rect = display.frame;
                let app_bounds = display.window_frame();
                host.push_guest_command(GuestCommand::Window {
                    task: task_id,
                    process,
                    rect,
                    app_bounds,
                    windowing_mode: WINDOWING_MODE_FULLSCREEN,
                });
                self.active = None;
                self.transitions += 1;
                Ok(json!({
                    "task_id": task_id,
                    "process": process,
                    "windowing_mode": WINDOWING_MODE_FULLSCREEN,
                    "in_multi_window_mode": false,
                    "bounds": rect_json(rect),
                    "app_bounds": rect_json(app_bounds),
                    "width_dp": width_dp,
                    "height_dp": height_dp,
                    "transitions": self.transitions,
                }))
            }
            "status" => {
                let wms = host.wms();
                let tasks: Vec<Value> = wms
                    .as_ref()
                    .map(|wms| {
                        let guard = wms
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        guard
                            .tasks()
                            .into_iter()
                            .map(|t| {
                                json!({
                                    "task_id": t.task_id,
                                    "windowing_mode": t.windowing_mode,
                                    "in_multi_window_mode": t.is_multi_window(),
                                    "bounds": rect_json(t.bounds),
                                    "app_bounds": rect_json(t.app_bounds),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(json!({
                    "module": MODULE_ID,
                    "profile": self.profile.as_str(),
                    "live_wms": wms.is_some(),
                    "active": self.active.is_some(),
                    "position": self.active.map(|a| a.position.as_str()),
                    "ratio": self.active.map(|a| a.ratio),
                    "windowing_mode": self
                        .active
                        .map(|a| a.record.windowing_mode)
                        .unwrap_or(WINDOWING_MODE_FULLSCREEN),
                    "bounds": self.active.map(|a| rect_json(a.record.bounds)),
                    "app_bounds": self.active.map(|a| rect_json(a.record.app_bounds)),
                    "transitions": self.transitions,
                    "tasks": tasks,
                }))
            }
            other => Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "unknown control op {other:?}; this module has {}",
                    CONTROL_OPS
                        .iter()
                        .map(|op| op.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }),
        }
    }

    fn report(&self) -> ModuleReport {
        let mut report = ModuleReport::of(manifest_ref(), self.profile);
        if self.profile == Profile::Real {
            report.reason = Some(
                "real multi-window: WMS task bounds + activity override Configuration via \
                 ServiceHost guest-command outbox (slice 1: single-task split enter_split/resize/exit)"
                    .to_string(),
            );
            report.details = Some(json!({
                "active": self.active.is_some(),
                "position": self.active.map(|a| a.position.as_str()),
                "ratio": self.active.map(|a| a.ratio),
                "windowing_mode": self
                    .active
                    .map(|a| a.record.windowing_mode)
                    .unwrap_or(WINDOWING_MODE_FULLSCREEN),
                "transitions": self.transitions,
            }));
        }
        report
    }
}
