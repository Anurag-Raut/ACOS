//! `sensors` — scripted sensor feeds through the real native service.
//!
//! Android's sensor stack is native: `SensorManager` (`libsensor`) talks to the
//! `sensorservice` daemon over `android.gui.ISensorServer`, gets an
//! `ISensorEventConnection` per event queue and reads `ASensorEvent` packets out
//! of a `BitTube`. [`crate::sensors`] serves that whole chain; this module is the
//! **content**: which sensors exist, what their values are, and the `play`
//! timelines that turn a script into an event stream at the rate the app asked
//! for.
//!
//! # Why the node is `core`'s and the feed is this module's
//!
//! `core` publishes the `sensorservice` node with no feed, which is the stock
//! answer of a device without sensors (an empty `getSensorList`). Two reasons
//! that cannot move:
//!
//! 1. A missing `sensorservice` name costs `SystemSensorManager.nativeCreate`
//!    two 5 s libbinder retry loops (W26 §1, ~12 s of the resume budget).
//! 2. An app resolves the `ISensorServer` proxy **once**, in its
//!    `SensorManager` constructor, and keeps it for the process's life. A hot
//!    attach that published a *second* binder would be invisible to an app that
//!    already booted.
//!
//! So attaching this module installs a [`SensorFeed`] into the node that is
//! already there (same binder identity), and detaching clears it: the node goes
//! back to the honest "no sensors on this device" answer, and every open event
//! connection stops.
//!
//! # Profiles
//!
//! * `stub` — the framework-exact sensor list with fixed values; a listener
//!   receives a constant stream at its requested rate. No control ops.
//! * `scripted` — the same, driven by the agent: `set_value`, `play`, `shake`,
//!   `tilt`, `enable`, `disable`.
//!
//! There is **no `real` profile** and this module does not list one: ACOS has no
//! sensor hardware and no HAL, so a measured accelerometer cannot exist. The
//! report says `scripted` and the verdict's `fidelity` row says `scripted`.
//!
//! # Honest limits (also in every report)
//!
//! * Values are agent-chosen, never measured.
//! * Delivery is not permission-gated. On a phone `sensorservice` only reports a
//!   sensor the app may use (the step counter needs `ACTIVITY_RECOGNITION`); the
//!   scripted feed reports every enabled sensor to every connection. The sensor
//!   table still carries the AOSP `requiredPermission` string, so an app that
//!   checks sees the real rule.
//! * There is no batching/FIFO: `maxBatchReportLatencyNs` is accepted and
//!   ignored, so `onFlushCompleted` arrives immediately (the flush-complete event
//!   is still delivered, exactly as `SensorService` does).
//! * A hot attach cannot change the sensor list an app has already read
//!   (`SystemSensorManager` caches `mSensorList` per process), so an app that
//!   booted before the attach keeps seeing no sensors until it recreates its
//!   `SensorManager` (an activity restart). Attaching at launch has no such
//!   limit.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::host::ServiceHost;
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass, ServiceProvide,
};
use crate::sensors::{
    self, name_by_type, type_by_name, Sample, SensorDef, SensorFeed, SensorHub, TYPE_ACCELEROMETER,
};

/// The module id (`acos_launch {"services": [{"id": "sensors"}]}`).
pub const MODULE_ID: &str = "sensors";

const CONTROL_OPS: [ControlOp; 9] = [
    ControlOp {
        name: "status",
        args: "none — the served sensor table, current values, live connections and counters",
    },
    ControlOp {
        name: "set_value",
        args: "{sensor: <name|type>, values: [..]} — set one sensor's value and deliver it now",
    },
    ControlOp {
        name: "play",
        args: "{sensor, samples: [{t_ms, values}], loop} — schedule a timeline of events",
    },
    ControlOp {
        name: "shake",
        args: "{sensor?: accelerometer, amplitude?: 12.0, count?: 12, period_ms?: 40}",
    },
    ControlOp {
        name: "tilt",
        args: "{angle_deg?: 30, axis?: x|y, duration_ms?: 250} — ramp the gravity vector",
    },
    ControlOp {
        name: "enable",
        args: "{sensor} — enable a sensor on every open event connection (as a listener would)",
    },
    ControlOp {
        name: "disable",
        args: "{sensor} — disable it again",
    },
    ControlOp {
        name: "events",
        args: "{limit?: 32} — the delivered events a verdict can cite",
    },
    ControlOp {
        name: "reset",
        args: "none — clear values, timelines and the event log",
    },
];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "scripted sensors (accelerometer, gyroscope, magnetometer, light, proximity, \
                  rotation vector, step counter) through the native sensorservice; no `real` profile \
                  (ACOS has no sensor hardware)",
        // The node is `core`'s (see the module docs); the manifest still claims the
        // name, because the *behaviour* behind it is this module's: with the
        // module off, `sensorservice` is the stock device-without-sensors reply.
        services: &[ServiceProvide {
            name: sensors::SERVICE_NAME,
            descriptor: sensors::SENSOR_SERVER_DESCRIPTOR,
        }],
        providers: &[],
        broadcasts: &[],
        windows: &[],
        system_apps: &[],
        requires: &[],
        requires_host_tools: &[],
        // `real` is deliberately absent: there is no sensor hardware.
        profiles: &[Profile::Stub, Profile::Scripted],
        budget: Budget {
            ram_mib: 2,
            startup_ms: 5,
        },
        attach: AttachMode::Hot,
        controls: &CONTROL_OPS,
        security: SecurityClass::InProcess,
    };
    &MANIFEST
}

/// The `sensors` module.
pub struct SensorsModule {
    profile: Profile,
    hub: Option<Arc<Mutex<SensorHub>>>,
    /// What the feed was built from (for the honest report).
    configured: Vec<i32>,
}

impl SensorsModule {
    /// A detached module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: Profile::Off,
            hub: None,
            configured: Vec::new(),
        }
    }

    /// The manifest (registry and tests).
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        manifest_ref()
    }

    fn scripted_only(&self, op: &str) -> Result<()> {
        if self.profile == Profile::Scripted {
            return Ok(());
        }
        Err(ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!(
                "{op:?} changes the feed, which is the `scripted` profile; this session is {} — \
                 attach `sensors:scripted`",
                self.profile
            ),
        })
    }

    fn hub(&self) -> Result<Arc<Mutex<SensorHub>>> {
        self.hub.clone().ok_or(ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: "module is off".to_string(),
        })
    }
}

impl Default for SensorsModule {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the feed a `config` asks for.
///
/// `config.sensors` narrows/reorders the default table by name (`accelerometer`,
/// `light`, ...); `config.values` sets initial values. Anything unknown is a
/// structured refusal, never a silent default: an agent that asks for `pressure`
/// must learn that this module cannot serve it.
fn build_feed(profile: Profile, config: &ModuleConfig) -> Result<SensorFeed> {
    let mut unknown: Vec<String> = Vec::new();
    let default_table = sensors::default_sensors();
    let mut feed = if config.get("sensors").is_some() {
        let mut selected: Vec<SensorDef> = Vec::new();
        for name in config.string_array("sensors") {
            let Some(sensor_type) = type_by_name(&name) else {
                unknown.push(name);
                continue;
            };
            match default_table
                .iter()
                .find(|sensor| sensor.sensor_type == sensor_type)
            {
                Some(sensor) => selected.push(sensor.clone()),
                None => unknown.push(name),
            }
        }
        if !unknown.is_empty() {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "config.sensors names {unknown:?}, which this module does not serve; it serves \
                     accelerometer, magnetometer, gyroscope, light, proximity, rotation_vector, \
                     step_counter"
                ),
            });
        }
        if selected.is_empty() {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: "config.sensors selected no sensor; an empty feed is the `off` profile, not \
                         an attached one"
                    .to_string(),
            });
        }
        SensorFeed::new(selected)
    } else {
        SensorFeed::new(default_table)
    };

    if let Some(values) = config.get("values") {
        let Some(object) = values.as_object() else {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("config.values must be an object of sensor -> array, got {values}"),
            });
        };
        for (name, value) in object {
            let Some(sensor_type) = type_by_name(name) else {
                return Err(ServiceError::ModuleRejected {
                    module: MODULE_ID,
                    detail: format!("config.values names unknown sensor {name:?}"),
                });
            };
            let Some(sensor) = feed.sensor_by_type(sensor_type) else {
                return Err(ServiceError::ModuleRejected {
                    module: MODULE_ID,
                    detail: format!(
                        "config.values sets {name:?}, which this feed does not serve (config.sensors \
                         narrowed the table)"
                    ),
                });
            };
            let Some(list) = value.as_array() else {
                return Err(ServiceError::ModuleRejected {
                    module: MODULE_ID,
                    detail: format!("config.values.{name} must be an array of numbers, got {value}"),
                });
            };
            let mut sample = [0.0f32; 16];
            if list.len() > 16 {
                return Err(ServiceError::ModuleRejected {
                    module: MODULE_ID,
                    detail: format!("config.values.{name} has {} values; a sensor event carries 16", list.len()),
                });
            }
            for (index, item) in list.iter().enumerate() {
                let Some(number) = item.as_f64() else {
                    return Err(ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!("config.values.{name}[{index}] must be a number, got {item}"),
                    });
                };
                sample[index] = number as f32;
            }
            let handle = sensor.handle;
            feed.values.insert(handle, sample);
        }
    }
    let _ = profile;
    Ok(feed)
}

/// Parse `values: [..]` into a 16-float event payload.
fn parse_values(values: Option<&Value>, method: &str) -> Result<[f32; 16]> {
    let mut out = [0.0f32; 16];
    let Some(values) = values else {
        return Err(ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!("{method} needs `values` (an array of up to 16 numbers)"),
        });
    };
    let Some(list) = values.as_array() else {
        return Err(ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!("{method}: `values` must be an array of numbers, got {values}"),
        });
    };
    if list.is_empty() || list.len() > 16 {
        return Err(ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!("{method}: `values` needs 1..=16 entries, got {}", list.len()),
        });
    }
    for (index, item) in list.iter().enumerate() {
        let Some(number) = item.as_f64() else {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("{method}: values[{index}] must be a number, got {item}"),
            });
        };
        out[index] = number as f32;
    }
    Ok(out)
}

impl Module for SensorsModule {
    fn manifest(&self) -> &'static ModuleManifest {
        manifest_ref()
    }

    fn attach(
        &mut self,
        host: &mut ServiceHost,
        profile: Profile,
        config: &ModuleConfig,
    ) -> Result<()> {
        if !matches!(profile, Profile::Stub | Profile::Scripted) {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "{profile} is not implemented: ACOS has no sensor hardware and no sensor HAL, \
                     so `real` would be a fake measurement; use `stub` or `scripted`"
                ),
            });
        }
        let feed = build_feed(profile, config)?;
        let configured: Vec<i32> = feed.sensors.iter().map(|sensor| sensor.sensor_type).collect();
        // The node `core` published. It is a dynamic (`raw_transact`-served) node,
        // so it is configured through `dynamic_object_mut` + the documented
        // `as_any_mut` downcast seam: no second registration, so an app's
        // `ISensorServer` proxy stays valid across a hot attach.
        let server = host
            .dynamic_object_mut(sensors::SENSOR_SERVER_DESCRIPTOR)
            .and_then(|object| object.as_any_mut())
            .and_then(|any| any.downcast_mut::<sensors::SensorServer>())
            .ok_or_else(|| ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: "the `sensorservice` node is not on this host: `core` publishes it and                          must be attached first"
                    .to_string(),
            })?;
        server.install_feed(feed);
        self.hub = Some(server.hub());
        self.configured = configured;
        self.profile = profile;
        Ok(())
    }

    fn detach(&mut self, host: &mut ServiceHost) -> Result<()> {
        // Stop the writer before the feed is cleared, so no event can be
        // written after detach returns. The node itself stays (it is `core`'s):
        // it answers the stock no-sensor reply again, which is what the app then
        // sees, and every open connection is dropped.
        if let Some(server) = host
            .dynamic_object_mut(sensors::SENSOR_SERVER_DESCRIPTOR)
            .and_then(|object| object.as_any_mut())
            .and_then(|any| any.downcast_mut::<sensors::SensorServer>())
        {
            server.clear_feed();
        }
        self.hub = None;
        self.configured.clear();
        self.profile = Profile::Off;
        Ok(())
    }

    fn control(&mut self, op: &str, args: &Value) -> Result<Value> {
        let hub = self.hub()?;
        match op {
            "status" => Ok(hub
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .status_json()),
            "events" => {
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|n| n as usize)
                    .unwrap_or(32);
                Ok(hub
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .events_json(limit))
            }
            "reset" => {
                let mut guard = hub
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard.reset();
                Ok(json!({"reset": true}))
            }
            "set_value" => {
                self.scripted_only(op)?;
                let mut guard = hub
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let sensor = guard.resolve_sensor(args.get("sensor"))?;
                let values = parse_values(args.get("values"), op)?;
                let now = sensors::now_ns();
                let reached = guard.set_value(sensor.handle, values, now)?;
                Ok(json!({
                    "applied": true,
                    "sensor": name_by_type(sensor.sensor_type),
                    "handle": sensor.handle,
                    "values": values.iter().take_while(|value| **value != 0.0).count(),
                    "delivered_to": reached,
                }))
            }
            "play" => {
                self.scripted_only(op)?;
                let looped = args.get("loop").and_then(Value::as_bool).unwrap_or(false);
                let samples = parse_samples(args)?;
                let count = samples.len();
                let duration = samples.last().map(|sample| sample.t_ms).unwrap_or(0);
                let mut guard = hub
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let sensor = guard.resolve_sensor(args.get("sensor"))?;
                let now = sensors::now_ns();
                guard.play(sensor.handle, samples, looped, now)?;
                Ok(json!({
                    "scheduled": count,
                    "loop": looped,
                    "duration_ms": duration,
                    "sensor": name_by_type(sensor.sensor_type),
                    "handle": sensor.handle,
                }))
            }
            "shake" => {
                self.scripted_only(op)?;
                let amplitude = args
                    .get("amplitude")
                    .and_then(Value::as_f64)
                    .unwrap_or(12.0) as f32;
                let count = args
                    .get("count")
                    .and_then(Value::as_u64)
                    .unwrap_or(12)
                    .clamp(2, 400) as usize;
                let period_ms = args
                    .get("period_ms")
                    .and_then(Value::as_i64)
                    .unwrap_or(40)
                    .max(1);
                let mut samples = Vec::with_capacity(count);
                for index in 0..count {
                    let sign = if index % 2 == 0 { 1.0 } else { -1.0 };
                    let mut values = [0.0f32; 16];
                    values[0] = sign * amplitude;
                    values[1] = 0.0;
                    values[2] = 9.81;
                    samples.push(Sample {
                        t_ms: index as i64 * period_ms,
                        values,
                    });
                }
                let duration = samples.last().map(|sample| sample.t_ms).unwrap_or(0);
                let mut guard = hub
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let sensor = guard.resolve_sensor(Some(&json!("accelerometer")))?;
                if let Some(requested) = args.get("sensor") {
                    let requested = guard.resolve_sensor(Some(requested))?;
                    if requested.sensor_type != TYPE_ACCELEROMETER {
                        return Err(ServiceError::ModuleRejected {
                            module: MODULE_ID,
                            detail: format!(
                                "`shake` drives the accelerometer, not {}",
                                name_by_type(requested.sensor_type)
                            ),
                        });
                    }
                }
                let now = sensors::now_ns();
                guard.play(sensor.handle, samples, false, now)?;
                Ok(json!({
                    "scheduled": count,
                    "amplitude": amplitude,
                    "period_ms": period_ms,
                    "duration_ms": duration,
                    "sensor": "accelerometer",
                }))
            }
            "tilt" => {
                self.scripted_only(op)?;
                let angle = args
                    .get("angle_deg")
                    .and_then(Value::as_f64)
                    .unwrap_or(30.0) as f32;
                let axis = args
                    .get("axis")
                    .and_then(Value::as_str)
                    .unwrap_or("x")
                    .to_string();
                if axis != "x" && axis != "y" {
                    return Err(ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!("`tilt` axis must be \"x\" or \"y\", got {axis:?}"),
                    });
                }
                let duration_ms = args
                    .get("duration_ms")
                    .and_then(Value::as_i64)
                    .unwrap_or(250)
                    .max(1);
                let steps = 5i64;
                let gravity = 9.81f32;
                let mut samples = Vec::with_capacity(steps as usize);
                for index in 1..=steps {
                    let fraction = index as f32 / steps as f32;
                    let radians = (angle * fraction).to_radians();
                    let mut values = [0.0f32; 16];
                    let component = gravity * radians.sin();
                    values[0] = if axis == "x" { component } else { 0.0 };
                    values[1] = if axis == "y" { component } else { 0.0 };
                    values[2] = gravity * radians.cos();
                    samples.push(Sample {
                        t_ms: index * duration_ms / steps,
                        values,
                    });
                }
                let mut guard = hub
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let sensor = guard.resolve_sensor(Some(&json!("accelerometer")))?;
                let now = sensors::now_ns();
                guard.play(sensor.handle, samples, false, now)?;
                Ok(json!({
                    "scheduled": steps,
                    "angle_deg": angle,
                    "axis": axis,
                    "duration_ms": duration_ms,
                }))
            }
            "enable" | "disable" => {
                self.scripted_only(op)?;
                let enabled = op == "enable";
                let period_ns = args
                    .get("period_ns")
                    .and_then(Value::as_i64)
                    .unwrap_or(20_000_000)
                    .max(1_000_000);
                let mut guard = hub
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let sensor = guard.resolve_sensor(args.get("sensor"))?;
                let connections = guard.force_enable(sensor.handle, enabled, period_ns);
                Ok(json!({
                    "sensor": name_by_type(sensor.sensor_type),
                    "handle": sensor.handle,
                    "enabled": enabled,
                    "connections": connections,
                }))
            }
            other => Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "unknown control op {other:?}; this module implements {}",
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
        let manifest = manifest_ref();
        let mut report = ModuleReport::of(manifest, self.profile);
        if self.profile.is_attached() {
            let summary = self
                .hub
                .as_ref()
                .map(|hub| {
                    hub.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .summary()
                })
                .unwrap_or_else(|| "feed installed".to_string());
            report.reason = Some(format!(
                "{} profile: {summary}; values are scripted, not measured, and delivery is not \
                 permission-gated",
                self.profile
            ));
        }
        report
    }
}

/// Parse `samples: [{t_ms, values}]`.
fn parse_samples(args: &Value) -> Result<Vec<Sample>> {
    let Some(samples) = args.get("samples") else {
        return Err(ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: "`play` needs `samples` (an array of {t_ms, values})".to_string(),
        });
    };
    let Some(list) = samples.as_array() else {
        return Err(ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!("`play`: samples must be an array, got {samples}"),
        });
    };
    if list.is_empty() {
        return Err(ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: "`play` needs at least one sample".to_string(),
        });
    }
    if list.len() > 4096 {
        return Err(ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!("`play` scheduled {} samples; the ceiling is 4096", list.len()),
        });
    }
    let mut out = Vec::with_capacity(list.len());
    let mut last = -1i64;
    for (index, item) in list.iter().enumerate() {
        let Some(object) = item.as_object() else {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("`play`: samples[{index}] must be an object, got {item}"),
            });
        };
        for key in object.keys() {
            if !matches!(key.as_str(), "t_ms" | "values") {
                return Err(ServiceError::ModuleRejected {
                    module: MODULE_ID,
                    detail: format!(
                        "`play`: samples[{index}] has unknown field {key:?}; use t_ms and values"
                    ),
                });
            }
        }
        let t_ms = object
            .get("t_ms")
            .and_then(Value::as_i64)
            .ok_or_else(|| ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("`play`: samples[{index}] needs an integer t_ms"),
            })?;
        if t_ms < 0 {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("`play`: samples[{index}].t_ms must be >= 0, got {t_ms}"),
            });
        }
        if t_ms < last {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "`play`: samples[{index}].t_ms = {t_ms} is before the previous sample ({last}); \
                     samples must be in time order"
                ),
            });
        }
        last = t_ms;
        let values = parse_values(object.get("values"), "play")?;
        out.push(Sample { t_ms, values });
    }
    Ok(out)
}
