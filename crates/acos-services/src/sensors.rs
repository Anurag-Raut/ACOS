//! The native sensor service: `android.gui.ISensorServer` +
//! `android.gui.ISensorEventConnection`.
//!
//! Android's `SensorManager` is *not* a Java system service. `SystemSensorManager`
//! calls `nativeCreate`, which builds `android::SensorManager`
//! (`frameworks/native/libs/sensor/SensorManager.cpp`), which talks to the
//! `sensorservice` native daemon over binder:
//!
//! ```text
//! SensorManager::assertStateLocked
//!   -> getService("sensorservice")                  [libbinder, 5 s retries/attempt]
//!   -> ISensorServer.getSensorList(opPackage)       -> Vector<Sensor>
//! SensorManager::createEventQueue
//!   -> ISensorServer.createSensorEventConnection(String8 opPackage, int mode,
//!                                                 String16 package, String16 tag)
//!                                                    -> ISensorEventConnection
//! SensorEventQueue::onFirstRef
//!   -> ISensorEventConnection.getSensorChannel()     -> BitTube (TWO fds)
//! SensorEventQueue::enableSensor (per registerListener)
//!   -> ISensorEventConnection.enableDisable(handle, enabled, samplingPeriodNs, ...)
//!   -> ISensorEventConnection.setEventRate(handle, ns)      [rate changes]
//!   -> ISensorEventConnection.flush()                       [SensorManager.flush]
//! ```
//!
//! # Wire evidence (API-34 image, not the tag where they disagree)
//!
//! * Transaction codes are the enum order of `native/libs/sensor/ISensorServer.cpp`
//!   (`GET_SENSOR_LIST = FIRST_CALL_TRANSACTION` = 1, `CREATE_SENSOR_EVENT_CONNECTION`
//!   = 2, `ENABLE_DATA_INJECTION` = 3, `GET_DYNAMIC_SENSOR_LIST` = 4,
//!   `CREATE_SENSOR_DIRECT_CONNECTION` = 5, `SET_OPERATION_PARAMETER` = 6,
//!   `GET_RUNTIME_SENSOR_LIST` = 7, `ENABLE_REPLAY_DATA_INJECTION` = 8,
//!   `ENABLE_HAL_BYPASS_REPLAY_DATA_INJECTION` = 9) and of
//!   `ISensorEventConnection.cpp` (`GET_SENSOR_CHANNEL` = 1, `ENABLE_DISABLE` = 2,
//!   `SET_EVENT_RATE` = 3, `FLUSH_SENSOR` = 4, `CONFIGURE_CHANNEL` = 5,
//!   `DESTROY` = 6). `ISensorServer` is a **hand-written** binder interface: its
//!   proxies read the reply with no `binder::Status`/exception header, unlike the
//!   AIDL `android.gui.ISurfaceComposer` next door. Every reply below therefore
//!   starts at its payload (this build's earlier empty-list answer wrote an
//!   exception word first, which the client was reading as the count).
//! * `getSensorList`'s reply is `writeUint32(count)` and then, per sensor,
//!   `Parcel::write(Flattenable)` = `writeInt32(flattenedSize)` + the flattened
//!   `Sensor` (`BpSensorServer::getSensorList` → `reply.read(s)`); the flattened
//!   field order and types are `Sensor::flatten`/`unflatten`
//!   (`libs/sensor/Sensor.cpp:534-609`): `String8 name`, `String8 vendor`, then
//!   i32 `version`, i32 `handle`, i32 `type`, f32 `minValue`, f32 `maxValue`,
//!   f32 `resolution`, f32 `power`, i32 `minDelay`, u32 `fifoReservedEventCount`,
//!   u32 `fifoMaxEventCount`, `String8 stringType`, `String8 requiredPermission`,
//!   bool(1 byte) `requiredPermissionRuntime`, i32 `requiredAppOp`,
//!   i32 `maxDelay`, u32 `flags`, 16-byte `uuid`, i32 `id`. A `String8` is
//!   `u32 byteLength` + bytes + padding to 4 bytes (the length is **not**
//!   NUL-terminated and is not the `-1` or doubled encoding `Parcel.writeString8`
//!   uses).
//! * An event on the `BitTube` is one `ASensorEvent`; the image's
//!   `SensorEventQueue::read` passes `objSize = 0x68` (104) to
//!   `BitTube::recvObjects` (`libsensor.so` 0x11dbd), so one `send(2)` message is
//!   one 104-byte event and `BitTube::recvObjects`' "partial events" assertion
//!   cannot fire. Layout (all little-endian, from the `ASensorEvent` union in
//!   `hardware/libhardware/include/hardware/sensors.h` and the field reads in
//!   `core/jni/android_hardware_SensorManager.cpp:403-450`): i32 `version`(104),
//!   i32 `sensor`(handle), i32 `type`, i32 pad, i64 `timestamp`, 16 × f32 `data`,
//!   u32 `flags`, 12 bytes `reserved1`.
//! * `getSensorChannel`'s reply is `gui::BitTube::writeToParcel`: two
//!   `writeDupFileDescriptor` calls (receive first, then send) and **nothing
//!   else** — no AIDL present flag. `BpSensorEventConnection::getSensorChannel`
//!   does `new BitTube(reply)`, i.e. `readFileDescriptor` twice.
//! * `enableDisable`/`setEventRate`/`flush`/`configureChannel` answer one
//!   `status_t` int32 each; `DESTROY` answers nothing.
//!
//! # What is scripted, and what that means
//!
//! There is no sensor hardware. With no feed installed the node answers the
//! **stock answer of a device without sensors**: an empty list, and a
//! `createSensorEventConnection` that reports the named gap (today's W26
//! behaviour). `core` publishes the node in that state because a `null`
//! `sensorservice` costs two 5 s libbinder retry loops inside
//! `SystemSensorManager.nativeCreate` (W26 §1), and because a module attached to
//! a **live** session must not invalidate a binder the app already holds — the
//! [`crate::modules::sensors`] module therefore *fills this node* instead of
//! publishing a second one.
//!
//! A scripted feed supplies the sensor table, the current values and any
//! scheduled (`play`) event timelines. Values are agent-chosen, never measured:
//! the module's report and every verdict row for it say `scripted`, and the
//! `fidelity` field of the module report says so too.

use std::collections::{BTreeMap, HashMap, VecDeque};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use acos_aidl::MethodInfo;
use acos_parcel::{BinderId, Parcel};
use rustix::net::SendFlags;
use serde_json::{json, Value};

use crate::error::{Result as SResult, ServiceError};
use crate::host::{CallResult, ServiceHost, ServiceObject};

/// `ServiceManager` name (`SensorManager::waitForSensorService`).
pub const SERVICE_NAME: &str = "sensorservice";
/// `ISensorServer::getInterfaceDescriptor()` (image `.rodata`, UTF-16).
pub const SENSOR_SERVER_DESCRIPTOR: &str = "android.gui.SensorServer";
/// `ISensorEventConnection::getInterfaceDescriptor()`.
pub const SENSOR_EVENT_CONNECTION_DESCRIPTOR: &str = "android.gui.SensorEventConnection";

/// `ISensorServer` transaction codes (`ISensorServer.cpp` enum order).
pub mod server_codes {
    /// `getSensorList(String16 opPackageName)` → `Vector<Sensor>`.
    pub const GET_SENSOR_LIST: u32 = 1;
    /// `createSensorEventConnection(String8, int mode, String16, String16)`.
    pub const CREATE_SENSOR_EVENT_CONNECTION: u32 = 2;
    /// `isDataInjectionEnabled()` → `int`.
    pub const IS_DATA_INJECTION_ENABLED: u32 = 3;
    /// `getDynamicSensorList(String16)` → `Vector<Sensor>`.
    pub const GET_DYNAMIC_SENSOR_LIST: u32 = 4;
    /// `createSensorDirectConnection(...)` → `ISensorEventConnection`.
    pub const CREATE_SENSOR_DIRECT_CONNECTION: u32 = 5;
    /// `setOperationParameter(int, int, float[], int[])` → `int`.
    pub const SET_OPERATION_PARAMETER: u32 = 6;
    /// `getRuntimeSensorList(String16, int deviceId)` → `Vector<Sensor>`.
    pub const GET_RUNTIME_SENSOR_LIST: u32 = 7;
    /// `isReplayDataInjectionEnabled()` → `int`.
    pub const IS_REPLAY_DATA_INJECTION_ENABLED: u32 = 8;
    /// `isHalBypassReplayDataInjectionEnabled()` → `int`.
    pub const IS_HAL_BYPASS_REPLAY_DATA_INJECTION_ENABLED: u32 = 9;
}

// W26 pin names (the codes the earlier placeholder named `SENSOR_*`). They are
// the same numbers as [`server_codes`]; the aliases keep the existing evidence
// test and its citations true, and give the one place a code is defined.
/// `getSensorList` (W26 name).
pub const SENSOR_GET_SENSOR_LIST: u32 = server_codes::GET_SENSOR_LIST;
/// `createSensorEventConnection` (W26 name).
pub const SENSOR_CREATE_EVENT_CONNECTION: u32 = server_codes::CREATE_SENSOR_EVENT_CONNECTION;
/// The no-argument `isDataInjectionEnabled()` (W26 name).
pub const SENSOR_NOARG_INT: u32 = server_codes::IS_DATA_INJECTION_ENABLED;
/// `getDynamicSensorList` (W26 name).
pub const SENSOR_GET_DYNAMIC_SENSOR_LIST: u32 = server_codes::GET_DYNAMIC_SENSOR_LIST;
/// `createSensorDirectConnection` (W26 name).
pub const SENSOR_CREATE_DIRECT_CONNECTION: u32 = server_codes::CREATE_SENSOR_DIRECT_CONNECTION;

/// `ISensorEventConnection` transaction codes (`ISensorEventConnection.cpp`).
pub mod connection_codes {
    /// `getSensorChannel()` → `gui::BitTube` (two fds).
    pub const GET_SENSOR_CHANNEL: u32 = 1;
    /// `enableDisable(int handle, bool enabled, nsecs_t samplingPeriodNs,
    ///  nsecs_t maxBatchReportLatencyNs, int reservedFlags)` → `status_t`.
    pub const ENABLE_DISABLE: u32 = 2;
    /// `setEventRate(int handle, nsecs_t ns)` → `status_t`.
    pub const SET_EVENT_RATE: u32 = 3;
    /// `flush()` → `status_t`.
    pub const FLUSH_SENSOR: u32 = 4;
    /// `configureChannel(int handle, int rateLevel)` → `int32_t`.
    pub const CONFIGURE_CHANNEL: u32 = 5;
    /// `destroy()` — oneway, no reply.
    pub const DESTROY: u32 = 6;
}

/// `sizeof(ASensorEvent)` in the API-34 image (`SensorEventQueue::read`'s
/// `objSize` immediate, `libsensor.so` 0x11dbd).
pub const EVENT_SIZE: usize = 104;

/// Offset of `ASensorEvent::version`.
pub const EVENT_OFFSET_VERSION: usize = 0;
/// Offset of the sensor handle (`ASensorEvent::sensor`).
pub const EVENT_OFFSET_HANDLE: usize = 4;
/// Offset of `ASensorEvent::type`.
pub const EVENT_OFFSET_TYPE: usize = 8;
/// Offset of `ASensorEvent::timestamp` (ns, `CLOCK_BOOTTIME`).
pub const EVENT_OFFSET_TIMESTAMP: usize = 16;
/// Offset of `ASensorEvent::data[16]` (also `u64.step_counter`, `meta_data`, ...).
pub const EVENT_OFFSET_DATA: usize = 24;
/// Offset of `ASensorEvent::flags`.
pub const EVENT_OFFSET_FLAGS: usize = 88;

/// `SENSOR_TYPE_ACCELEROMETER`.
pub const TYPE_ACCELEROMETER: i32 = 1;
/// `SENSOR_TYPE_MAGNETIC_FIELD`.
pub const TYPE_MAGNETIC_FIELD: i32 = 2;
/// `SENSOR_TYPE_GYROSCOPE`.
pub const TYPE_GYROSCOPE: i32 = 4;
/// `SENSOR_TYPE_LIGHT`.
pub const TYPE_LIGHT: i32 = 5;
/// `SENSOR_TYPE_PROXIMITY`.
pub const TYPE_PROXIMITY: i32 = 8;
/// `SENSOR_TYPE_ROTATION_VECTOR`.
pub const TYPE_ROTATION_VECTOR: i32 = 11;
/// `SENSOR_TYPE_STEP_COUNTER`.
pub const TYPE_STEP_COUNTER: i32 = 19;
/// `SENSOR_TYPE_META_DATA` (the type a flush-complete event carries).
pub const TYPE_META_DATA: i32 = 0;

/// `SENSOR_FLAG_WAKE_UP`.
pub const FLAG_WAKE_UP: u32 = 0x1;
/// `SENSOR_FLAG_ON_CHANGE_MODE` (`REPORTING_MODE_ON_CHANGE`; continuous is 0).
pub const FLAG_ON_CHANGE_MODE: u32 = 0x2;
/// `SENSOR_FLAG_ONE_SHOT_MODE`.
pub const FLAG_ONE_SHOT_MODE: u32 = 0x4;
/// `META_DATA_FLUSH_COMPLETE` (`meta_data.what`).
pub const META_DATA_FLUSH_COMPLETE: i32 = 1;

/// `status_t` NO_ERROR, the answer of an accepted enable/disable/flush.
pub const NO_ERROR: i32 = 0;
/// `status_t` BAD_VALUE.
pub const BAD_VALUE: i32 = -22;

/// `CLOCK_BOOTTIME` in nanoseconds — what `ASensorEvent::timestamp` carries
/// (`elapsedRealtimeNanos`, i.e. time since boot including suspend).
#[must_use]
pub fn now_ns() -> i64 {
    use rustix::time::{clock_gettime, ClockId};
    // `CLOCK_BOOTTIME` is `ClockId::Boottime`; a kernel without it degrades to
    // `MONOTONIC`, which is the same value on a machine that never suspends.
    let ts = clock_gettime(ClockId::Boottime);
    ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64
}

// ---------------------------------------------------------------------------
// The sensor table
// ---------------------------------------------------------------------------

/// One sensor, in the field order `Sensor::flatten` writes.
#[derive(Clone, Debug, PartialEq)]
pub struct SensorDef {
    /// `mHandle`: what `enableDisable`/events name.
    pub handle: i32,
    /// `mType` (`SENSOR_TYPE_*`).
    pub sensor_type: i32,
    /// `mName` (`"BMI160 Accelerometer"`).
    pub name: String,
    /// `mVendor`.
    pub vendor: String,
    /// `mVersion`.
    pub version: i32,
    /// `mMinValue` (a `Sensor` field the HAL never sets; 0 like AOSP).
    pub min_value: f32,
    /// `mMaxValue`.
    pub max_value: f32,
    /// `mResolution`.
    pub resolution: f32,
    /// `mPower`.
    pub power: f32,
    /// `mMinDelay` (µs).
    pub min_delay: i32,
    /// `mFifoReservedEventCount`.
    pub fifo_reserved_event_count: u32,
    /// `mFifoMaxEventCount`.
    pub fifo_max_event_count: u32,
    /// `mStringType` (`"android.sensor.accelerometer"`).
    pub string_type: String,
    /// `mRequiredPermission` (empty when none).
    pub required_permission: String,
    /// `mRequiredAppOp` (-1 when none, as `Sensor.cpp` defaults it).
    pub required_app_op: i32,
    /// `mMaxDelay` (µs).
    pub max_delay: i32,
    /// `mFlags` (reporting mode + wake-up bit).
    pub flags: u32,
    /// `mUuid` (all zero for a built-in sensor).
    pub uuid: [u8; 16],
    /// `mId`.
    pub id: i32,
}

impl SensorDef {
    /// `true` when the sensor reports continuously (otherwise on change).
    #[must_use]
    pub fn is_continuous(&self) -> bool {
        self.flags & (FLAG_ON_CHANGE_MODE | FLAG_ONE_SHOT_MODE | 0x8) == 0
    }

    /// The `String8` encoding `Sensor::flatten` uses: `u32 length` + bytes +
    /// padding to 4 bytes. Not `Parcel.writeString8` (that is `writeString16`'s
    /// sibling, with the `-1`/doubled-length conventions this layout does not
    /// have).
    fn write_string8(out: &mut Vec<u8>, text: &str) {
        let bytes = text.as_bytes();
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
        let pad = (4 - (bytes.len() % 4)) % 4;
        out.extend(std::iter::repeat_n(0u8, pad));
    }

    /// `Sensor::flatten`'s bytes, without the leading `writeInt32(size)` that
    /// `Parcel::write(Flattenable)` adds.
    #[must_use]
    pub fn flatten(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(160);
        Self::write_string8(&mut out, &self.name);
        Self::write_string8(&mut out, &self.vendor);
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&self.handle.to_le_bytes());
        out.extend_from_slice(&self.sensor_type.to_le_bytes());
        out.extend_from_slice(&self.min_value.to_le_bytes());
        out.extend_from_slice(&self.max_value.to_le_bytes());
        out.extend_from_slice(&self.resolution.to_le_bytes());
        out.extend_from_slice(&self.power.to_le_bytes());
        out.extend_from_slice(&self.min_delay.to_le_bytes());
        out.extend_from_slice(&self.fifo_reserved_event_count.to_le_bytes());
        out.extend_from_slice(&self.fifo_max_event_count.to_le_bytes());
        Self::write_string8(&mut out, &self.string_type);
        Self::write_string8(&mut out, &self.required_permission);
        // `bool` is one byte in `FlattenableUtils::write` (no alignment).
        out.push(0);
        out.extend_from_slice(&self.required_app_op.to_le_bytes());
        out.extend_from_slice(&self.max_delay.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.uuid);
        out.extend_from_slice(&self.id.to_le_bytes());
        out
    }

    /// The `getSensorList` element: `writeInt32(flattenedSize)` + `flatten` +
    /// zero padding to four bytes.
    ///
    /// The padding is not cosmetic: `Parcel::writeInplace(len)` reserves
    /// `pad_size(len)` bytes (`libsensor`'s `Sensor` is a `LightFlattenable`, so
    /// `Parcel::write` is `writeInt32(size)` + `writeInplace(size)`), and the
    /// client's `LightFlattenable` read advances by `pad_size(size)`. A flattened
    /// `Sensor` is **145** bytes for the default table (the `String8` fields pad
    /// individually, but the tail is `bool(1) + 4 + 4 + 4 + 16 + 4`), so without
    /// the pad every sensor after the first starts one byte late and the guest
    /// logs `Failed to read reply from getSensorList` and drops the whole list —
    /// which is exactly what the first ART run of this module showed.
    fn write_to_reply(&self, reply: &mut Parcel) -> SResult<()> {
        let flat = self.flatten();
        reply.write_int32(i32::try_from(flat.len()).map_err(|_| {
            ServiceError::Internal("sensors: flattened Sensor exceeds i32::MAX".into())
        })?);
        reply.write_raw_bytes(&flat);
        let pad = (4 - (flat.len() % 4)) % 4;
        if pad > 0 {
            reply.write_raw_bytes(&[0u8; 4][..pad]);
        }
        Ok(())
    }

    /// The `sensors` control answer for this sensor.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "handle": self.handle,
            "type": self.sensor_type,
            "name": self.name,
            "vendor": self.vendor,
            "string_type": self.string_type,
            "max_range": self.max_value,
            "resolution": self.resolution,
            "power": self.power,
            "min_delay_us": self.min_delay,
            "reporting": if self.is_continuous() { "continuous" } else { "on-change" },
            "wake_up": self.flags & FLAG_WAKE_UP != 0,
            "required_permission": self.required_permission,
        })
    }
}

fn sensor(
    handle: i32,
    sensor_type: i32,
    name: &str,
    vendor: &str,
    string_type: &str,
    max_value: f32,
    resolution: f32,
    power: f32,
    min_delay: i32,
    flags: u32,
) -> SensorDef {
    SensorDef {
        handle,
        sensor_type,
        name: name.to_string(),
        vendor: vendor.to_string(),
        version: 1,
        min_value: 0.0,
        max_value,
        resolution,
        power,
        min_delay,
        fifo_reserved_event_count: 0,
        fifo_max_event_count: 0,
        string_type: string_type.to_string(),
        required_permission: String::new(),
        required_app_op: -1,
        max_delay: 0,
        flags,
        uuid: [0u8; 16],
        id: handle - 1,
    }
}

/// The sensor list a phone of this shape reports, in the order a real
/// `sensorservice` publishes it (mostly `sensors.<board>.so` order; the numeric
/// handles are ACOS's, since a handle is opaque).
///
/// Ranges/resolutions/powers are the API-34 reference values a common phone
/// reports (they only have to be self-consistent: an app that clamps to
/// `getMaximumRange()` must not see a value outside it).
#[must_use]
pub fn default_sensors() -> Vec<SensorDef> {
    let mut step = sensor(
        6,
        TYPE_STEP_COUNTER,
        "Step Counter",
        "ACOS",
        "android.sensor.step_counter",
        4.0e9,
        1.0,
        0.5,
        0,
        FLAG_ON_CHANGE_MODE,
    );
    // `Sensor.cpp`'s ctor attaches the runtime permission to the step counter.
    step.required_permission = "android.permission.ACTIVITY_RECOGNITION".to_string();
    vec![
        sensor(
            1,
            TYPE_ACCELEROMETER,
            "BMI160 Accelerometer",
            "Bosch",
            "android.sensor.accelerometer",
            39.2266,
            0.001_5,
            0.18,
            5_000,
            0,
        ),
        sensor(
            2,
            TYPE_MAGNETIC_FIELD,
            "AK09918 Magnetometer",
            "AKM",
            "android.sensor.magnetic_field",
            4_911.0,
            0.15,
            0.1,
            10_000,
            0,
        ),
        sensor(
            3,
            TYPE_GYROSCOPE,
            "BMI160 Gyroscope",
            "Bosch",
            "android.sensor.gyroscope",
            34.906_586,
            0.001,
            0.13,
            5_000,
            0,
        ),
        sensor(
            4,
            TYPE_LIGHT,
            "STK3335 Ambient Light",
            "Sensortek",
            "android.sensor.light",
            65_535.0,
            1.0,
            0.1,
            0,
            FLAG_ON_CHANGE_MODE,
        ),
        sensor(
            5,
            TYPE_PROXIMITY,
            "STK3335 Proximity",
            "Sensortek",
            "android.sensor.proximity",
            5.0,
            5.0,
            0.1,
            0,
            FLAG_ON_CHANGE_MODE,
        ),
        sensor(
            7,
            TYPE_ROTATION_VECTOR,
            "Rotation Vector",
            "ACOS",
            "android.sensor.rotation_vector",
            1.0,
            1.0e-5,
            0.18,
            5_000,
            0,
        ),
        step,
    ]
}

/// The default value of each default sensor (`registerListener` before any
/// `set_value`): a phone lying flat and still.
#[must_use]
pub fn default_values(handle: i32, sensor_type: i32) -> [f32; 16] {
    let mut values = [0.0f32; 16];
    match sensor_type {
        TYPE_ACCELEROMETER => {
            values[0] = 0.0;
            values[1] = 0.0;
            values[2] = 9.81;
        }
        TYPE_MAGNETIC_FIELD => {
            values[1] = 45.0;
            values[2] = -30.0;
        }
        TYPE_GYROSCOPE => {}
        TYPE_LIGHT => values[0] = 100.0,
        TYPE_PROXIMITY => values[0] = 5.0,
        TYPE_ROTATION_VECTOR => {
            values[3] = 1.0;
        }
        TYPE_STEP_COUNTER => values[0] = 0.0,
        _ => {}
    }
    let _ = handle;
    values
}

/// `SENSOR_TYPE_*` -> the name the control ops accept (`accelerometer`).
#[must_use]
pub fn type_by_name(name: &str) -> Option<i32> {
    let name = name.trim();
    [
        (TYPE_ACCELEROMETER, "accelerometer"),
        (TYPE_MAGNETIC_FIELD, "magnetometer"),
        (TYPE_MAGNETIC_FIELD, "magnetic_field"),
        (TYPE_GYROSCOPE, "gyroscope"),
        (TYPE_LIGHT, "light"),
        (TYPE_PROXIMITY, "proximity"),
        (TYPE_ROTATION_VECTOR, "rotation_vector"),
        (TYPE_STEP_COUNTER, "step_counter"),
        (TYPE_STEP_COUNTER, "stepcounter"),
    ]
    .into_iter()
    .find(|(_, candidate)| *candidate == name)
    .map(|(sensor_type, _)| sensor_type)
}

/// The control name of a sensor type.
#[must_use]
pub fn name_by_type(sensor_type: i32) -> &'static str {
    match sensor_type {
        TYPE_ACCELEROMETER => "accelerometer",
        TYPE_MAGNETIC_FIELD => "magnetometer",
        TYPE_GYROSCOPE => "gyroscope",
        TYPE_LIGHT => "light",
        TYPE_PROXIMITY => "proximity",
        TYPE_ROTATION_VECTOR => "rotation_vector",
        TYPE_STEP_COUNTER => "step_counter",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------------
// Event encoding
// ---------------------------------------------------------------------------

/// One event as the `BitTube` carries it.
#[derive(Clone, Debug, PartialEq)]
pub struct SensorEvent {
    /// The sensor handle.
    pub handle: i32,
    /// The event type (`ASensorEvent::type`; `SENSOR_TYPE_META_DATA` for a
    /// flush-complete).
    pub sensor_type: i32,
    /// `CLOCK_BOOTTIME` nanoseconds.
    pub timestamp_ns: i64,
    /// `data[16]`; a step counter uses `values[0]` as a `u64` (AOSP's
    /// `u64.step_counter` union member, which is what the JNI reads for it).
    pub values: [f32; 16],
    /// `ASensorEvent::flags`.
    pub flags: u32,
}

impl SensorEvent {
    /// A plain sample event.
    #[must_use]
    pub fn sample(sensor: &SensorDef, timestamp_ns: i64, values: [f32; 16]) -> Self {
        Self {
            handle: sensor.handle,
            sensor_type: sensor.sensor_type,
            timestamp_ns,
            values,
            flags: 0,
        }
    }

    /// The `META_DATA_FLUSH_COMPLETE` event `flush()` answers with
    /// (`meta_data.what` then `meta_data.sensor`, both int32 in `data`).
    #[must_use]
    pub fn flush_complete(sensor: &SensorDef, timestamp_ns: i64) -> Self {
        let mut values = [0.0f32; 16];
        values[0] = META_DATA_FLUSH_COMPLETE as f32;
        values[1] = sensor.handle as f32;
        Self {
            handle: sensor.handle,
            sensor_type: TYPE_META_DATA,
            timestamp_ns,
            values,
            flags: 0,
        }
    }

    /// The 104 bytes one `send(2)` carries.
    #[must_use]
    pub fn encode(&self) -> [u8; EVENT_SIZE] {
        let mut out = [0u8; EVENT_SIZE];
        out[EVENT_OFFSET_VERSION..EVENT_OFFSET_VERSION + 4]
            .copy_from_slice(&(EVENT_SIZE as i32).to_le_bytes());
        out[EVENT_OFFSET_HANDLE..EVENT_OFFSET_HANDLE + 4]
            .copy_from_slice(&self.handle.to_le_bytes());
        out[EVENT_OFFSET_TYPE..EVENT_OFFSET_TYPE + 4]
            .copy_from_slice(&self.sensor_type.to_le_bytes());
        out[EVENT_OFFSET_TIMESTAMP..EVENT_OFFSET_TIMESTAMP + 8]
            .copy_from_slice(&self.timestamp_ns.to_le_bytes());
        if self.sensor_type == TYPE_STEP_COUNTER {
            // `ASensorEvent::u64.step_counter` at the first data word.
            let steps = self.values[0].max(0.0) as u64;
            out[EVENT_OFFSET_DATA..EVENT_OFFSET_DATA + 8].copy_from_slice(&steps.to_le_bytes());
        } else {
            for (index, value) in self.values.iter().enumerate() {
                let at = EVENT_OFFSET_DATA + index * 4;
                out[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
        out[EVENT_OFFSET_FLAGS..EVENT_OFFSET_FLAGS + 4].copy_from_slice(&self.flags.to_le_bytes());
        out
    }

    /// The recorded-evidence form (control `events` op, module report).
    #[must_use]
    pub fn to_json(&self) -> Value {
        let values: Vec<Value> = if self.sensor_type == TYPE_STEP_COUNTER {
            vec![json!(self.values[0].max(0.0) as u64)]
        } else {
            self.values
                .iter()
                .take_while(|value| **value != 0.0 || self.sensor_type == TYPE_PROXIMITY)
                .map(|value| json!(value))
                .collect()
        };
        json!({
            "sensor": name_by_type(self.sensor_type),
            "type": self.sensor_type,
            "handle": self.handle,
            "t_ms": self.timestamp_ns / 1_000_000,
            "values": if values.is_empty() { json!([0.0]) } else { json!(values) },
            "flush_complete": self.sensor_type == TYPE_META_DATA,
        })
    }
}

// ---------------------------------------------------------------------------
// The feed (what a module installs)
// ---------------------------------------------------------------------------

/// A scheduled sample of a `play` timeline, relative to the play's start.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    /// Offset from the timeline's start, in ms.
    pub t_ms: i64,
    /// Up to 16 values.
    pub values: [f32; 16],
}

/// What an attached sensors module installs into the node.
#[derive(Clone, Debug)]
pub struct SensorFeed {
    /// The sensors this feed serves (empty is never installed: an empty feed is
    /// the `off` state).
    pub sensors: Vec<SensorDef>,
    /// The current value per handle (`set_value`).
    pub values: BTreeMap<i32, [f32; 16]>,
    /// How many delivered events the ring keeps for evidence.
    pub log_capacity: usize,
}

impl SensorFeed {
    /// A feed over `sensors` with each sensor's `default_values`.
    #[must_use]
    pub fn new(sensors: Vec<SensorDef>) -> Self {
        let values = sensors
            .iter()
            .map(|sensor| (sensor.handle, default_values(sensor.handle, sensor.sensor_type)))
            .collect();
        Self {
            sensors,
            values,
            log_capacity: 64,
        }
    }

    /// The default feed (the seven sensors above).
    #[must_use]
    pub fn standard() -> Self {
        Self::new(default_sensors())
    }

    /// The sensor with `sensor_type`, if the feed serves it.
    #[must_use]
    pub fn sensor_by_type(&self, sensor_type: i32) -> Option<&SensorDef> {
        self.sensors
            .iter()
            .find(|sensor| sensor.sensor_type == sensor_type)
    }

    /// The sensor with `handle`.
    #[must_use]
    pub fn sensor_by_handle(&self, handle: i32) -> Option<&SensorDef> {
        self.sensors.iter().find(|sensor| sensor.handle == handle)
    }
}

// ---------------------------------------------------------------------------
// Per-connection state
// ---------------------------------------------------------------------------

/// One `ISensorEventConnection`'s live state (its `BitTube` and its enables).
#[derive(Debug)]
pub struct ConnectionState {
    /// The pool slot this connection occupies.
    pub slot: usize,
    /// The `BitTube` of this connection; `None` until the client stole the
    /// channel with `getSensorChannel`.
    pub tube: Option<crate::display_events::BitTube>,
    /// `enableDisable`/`setEventRate` state: handle -> sampling period (ns).
    pub enabled: HashMap<i32, i64>,
    /// Next due sample per handle (ns).
    pub next_due: HashMap<i32, i64>,
    /// The last value delivered per handle (on-change suppression).
    pub last: HashMap<i32, [f32; 16]>,
    /// Sensors this connection asked `enable` for, in request order.
    pub enable_log: Vec<(i32, bool, i64)>,
    /// `flush()` calls.
    pub flush_calls: u64,
    /// `true` after `destroy()` (or a slot reuse): the writer stops feeding it.
    pub destroyed: bool,
}

impl ConnectionState {
    fn new(slot: usize) -> Self {
        Self {
            slot,
            tube: None,
            enabled: HashMap::new(),
            next_due: HashMap::new(),
            last: HashMap::new(),
            enable_log: Vec::new(),
            flush_calls: 0,
            destroyed: false,
        }
    }

    /// `true` once the client owns a receive channel for this connection.
    #[must_use]
    pub fn has_channel(&self) -> bool {
        self.tube.is_some()
    }

    /// Write one event into this connection's tube.
    fn deliver(&self, event: &SensorEvent) -> bool {
        let Some(tube) = self.tube.as_ref() else {
            return false;
        };
        let Some(send) = tube.send_fd() else {
            return false;
        };
        let bytes = event.encode();
        rustix::net::send(send, &bytes, SendFlags::DONTWAIT | SendFlags::NOSIGNAL).is_ok()
    }
}

/// A `play` timeline: scheduled samples delivered as events.
#[derive(Debug)]
struct Timeline {
    handle: i32,
    samples: Vec<Sample>,
    looped: bool,
    started_ns: i64,
    next: usize,
    cycle: u64,
}

// ---------------------------------------------------------------------------
// The hub
// ---------------------------------------------------------------------------

/// The shared state the node, the event connections and the writer thread use.
#[derive(Debug, Default)]
pub struct SensorHub {
    /// `None` while the module is off: the node then answers the stock
    /// device-without-sensors reply.
    feed: Option<SensorFeed>,
    /// Live event connections.
    connections: Vec<Arc<Mutex<ConnectionState>>>,
    /// Scheduled timelines (`play`).
    timelines: Vec<Timeline>,
    /// Events actually written into a tube, in delivery order (ring).
    log: VecDeque<SensorEvent>,
    /// Total events written (not capped by the ring).
    delivered: u64,
    /// Events dropped because a tube was full (EAGAIN), as `BitTube::write` does.
    dropped: u64,
    /// `set_value` calls applied.
    set_value_calls: u64,
    /// `play` calls scheduled.
    play_calls: u64,
}

impl SensorHub {
    /// The installed feed, if any (evidence and control ops).
    #[must_use]
    pub fn has_feed(&self) -> bool {
        self.feed.is_some()
    }

    /// The served sensors (empty when off).
    #[must_use]
    pub fn sensors(&self) -> &[SensorDef] {
        self.feed.as_ref().map(|feed| feed.sensors.as_slice()).unwrap_or(&[])
    }

    /// The current value of `handle`.
    #[must_use]
    pub fn value(&self, handle: i32) -> Option<[f32; 16]> {
        self.feed
            .as_ref()
            .and_then(|feed| feed.values.get(&handle).copied())
    }

    /// The live connections, in creation order.
    #[must_use]
    pub fn connections(&self) -> &[Arc<Mutex<ConnectionState>>] {
        &self.connections
    }

    /// The sensor of `sensor_type`, if served.
    #[must_use]
    pub fn sensor_by_type(&self, sensor_type: i32) -> Option<&SensorDef> {
        self.feed
            .as_ref()
            .and_then(|feed| feed.sensor_by_type(sensor_type))
    }

    /// The sensor of `handle`, if served.
    #[must_use]
    pub fn sensor_by_handle(&self, handle: i32) -> Option<&SensorDef> {
        self.feed
            .as_ref()
            .and_then(|feed| feed.sensor_by_handle(handle))
    }

    /// Resolve a control argument (`"accelerometer"` or `1`) against the served
    /// table. A miss names what *is* served, never a silent default.
    pub fn resolve_sensor(&self, value: Option<&Value>) -> SResult<SensorDef> {
        let Some(value) = value else {
            return Err(ServiceError::ModuleRejected {
                module: "sensors",
                detail: "the op needs `sensor` (a name like \"accelerometer\" or an SENSOR_TYPE int)"
                    .to_string(),
            });
        };
        let sensor_type = match value {
            Value::String(name) => type_by_name(name),
            Value::Number(number) => number.as_i64().map(|n| n as i32),
            _ => None,
        };
        let Some(sensor_type) = sensor_type else {
            return Err(ServiceError::ModuleRejected {
                module: "sensors",
                detail: format!("unknown sensor {value}; use a name or an SENSOR_TYPE int"),
            });
        };
        self.sensor_by_type(sensor_type)
            .cloned()
            .ok_or_else(|| ServiceError::ModuleRejected {
                module: "sensors",
                detail: format!(
                    "{value} is not in this feed (served: {:?})",
                    self.sensors()
                        .iter()
                        .map(|sensor| name_by_type(sensor.sensor_type))
                        .collect::<Vec<_>>()
                ),
            })
    }

    /// `enable`/`disable` from the agent: as if a registered listener had asked.
    ///
    /// Returns how many open event connections the change reached. A connection
    /// that never stole its channel cannot receive events, so it is skipped.
    pub fn force_enable(&mut self, handle: i32, enabled: bool, period_ns: i64) -> usize {
        let now = now_ns();
        let mut reached = 0;
        for connection in &self.connections {
            let mut state = connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.destroyed || !state.has_channel() {
                continue;
            }
            if enabled {
                state.enabled.insert(handle, period_ns.max(1_000_000));
                state.next_due.insert(handle, now + period_ns.max(1_000_000));
            } else {
                state.enabled.remove(&handle);
                state.next_due.remove(&handle);
                state.last.remove(&handle);
            }
            reached += 1;
        }
        reached
    }

    /// `reset`: clear values to the feed's defaults, the timelines and the log,
    /// keeping the connections.
    pub fn reset(&mut self) {
        self.timelines.clear();
        self.log.clear();
        self.delivered = 0;
        self.dropped = 0;
        self.set_value_calls = 0;
        self.play_calls = 0;
        if let Some(feed) = self.feed.as_mut() {
            feed.values = feed
                .sensors
                .iter()
                .map(|sensor| (sensor.handle, default_values(sensor.handle, sensor.sensor_type)))
                .collect();
        }
        for connection in &self.connections {
            let mut state = connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.last.clear();
        }
    }

    /// Install a feed (the module's `attach`). Clears every earlier connection
    /// state so a re-attach starts clean.
    pub fn install(&mut self, feed: SensorFeed) {
        self.feed = Some(feed);
        self.timelines.clear();
        self.log.clear();
        self.delivered = 0;
        self.dropped = 0;
        self.set_value_calls = 0;
        self.play_calls = 0;
    }

    /// Remove the feed (the module's `detach`): the node is the stock
    /// no-sensor device again and every open event connection is dropped.
    pub fn clear(&mut self) {
        self.feed = None;
        self.timelines.clear();
        self.log.clear();
        for connection in &self.connections {
            connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .destroyed = true;
        }
        self.connections.clear();
        self.delivered = 0;
        self.dropped = 0;
        self.set_value_calls = 0;
        self.play_calls = 0;
    }

    /// Register a fresh connection for pool slot `slot`.
    fn open_connection(&mut self, slot: usize) -> Arc<Mutex<ConnectionState>> {
        self.connections
            .retain(|connection| {
                let mut state = connection
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.slot == slot {
                    state.destroyed = true;
                    return false;
                }
                !state.destroyed
            });
        let connection = Arc::new(Mutex::new(ConnectionState::new(slot)));
        self.connections.push(Arc::clone(&connection));
        connection
    }

    /// `set_value`: install the current value and deliver one event immediately
    /// to every connection that has the sensor enabled.
    pub fn set_value(&mut self, handle: i32, values: [f32; 16], now: i64) -> SResult<usize> {
        let Some(feed) = self.feed.as_mut() else {
            return Err(ServiceError::ModuleRejected {
                module: "sensors",
                detail: "the feed is off".into(),
            });
        };
        let Some(sensor) = feed.sensor_by_handle(handle).cloned() else {
            return Err(ServiceError::BadRequest {
                interface: SENSOR_SERVER_DESCRIPTOR.to_string(),
                method: "set_value".into(),
                detail: format!("no sensor has handle {handle}"),
            });
        };
        feed.values.insert(handle, values);
        self.set_value_calls += 1;
        let event = SensorEvent::sample(&sensor, now, values);
        Ok(self.deliver_to_enabled(&event))
    }

    /// Write `event` into every connection that enabled the sensor. Returns the
    /// number of connections it reached.
    fn deliver_to_enabled(&mut self, event: &SensorEvent) -> usize {
        let mut reached = 0;
        let mut dropped = 0;
        for connection in &self.connections {
            let state = connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.destroyed || !state.enabled.contains_key(&event.handle) {
                continue;
            }
            if state.deliver(event) {
                reached += 1;
            } else {
                dropped += 1;
            }
        }
        if reached > 0 {
            self.record(event);
        }
        self.dropped += dropped;
        reached
    }

    fn record(&mut self, event: &SensorEvent) {
        self.delivered += 1;
        let capacity = self
            .feed
            .as_ref()
            .map(|feed| feed.log_capacity)
            .unwrap_or(64);
        self.log.push_back(event.clone());
        while self.log.len() > capacity {
            self.log.pop_front();
        }
    }

    /// Deliver one event into one connection and record it (a flush-complete is
    /// that connection's, not every enabled connection's).
    fn deliver_to(
        &mut self,
        connection: &Arc<Mutex<ConnectionState>>,
        event: &SensorEvent,
    ) -> bool {
        let delivered = connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .deliver(event);
        if delivered {
            self.record(event);
        } else {
            self.dropped += 1;
        }
        delivered
    }

    /// Schedule `samples` for `handle` starting now.
    pub fn play(&mut self, handle: i32, samples: Vec<Sample>, looped: bool, now: i64) -> SResult<()> {
        if self.feed.as_ref().and_then(|feed| feed.sensor_by_handle(handle)).is_none() {
            return Err(ServiceError::BadRequest {
                interface: SENSOR_SERVER_DESCRIPTOR.to_string(),
                method: "play".into(),
                detail: format!("no sensor has handle {handle}"),
            });
        }
        if samples.is_empty() {
            return Err(ServiceError::BadRequest {
                interface: SENSOR_SERVER_DESCRIPTOR.to_string(),
                method: "play".into(),
                detail: "play needs at least one sample".into(),
            });
        }
        self.play_calls += 1;
        self.timelines.push(Timeline {
            handle,
            samples,
            looped,
            started_ns: now,
            next: 0,
            cycle: 0,
        });
        Ok(())
    }

    /// Emit every scheduled timeline sample that is due, and every periodic
    /// sample of a continuously reporting sensor. Returns the next time
    /// something is due (ns), or `None` when the hub is idle.
    fn tick(&mut self, now: i64) -> Option<i64> {
        if self.feed.is_none() {
            return None;
        }
        self.connections.retain(|connection| {
            !connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .destroyed
        });

        // 1. `play` timelines: each sample is one event, at its own offset.
        let mut due_events: Vec<SensorEvent> = Vec::new();
        let mut finished: Vec<usize> = Vec::new();
        let mut timeline_next: Option<i64> = None;
        for (index, timeline) in self.timelines.iter_mut().enumerate() {
            let Some(sensor) = self
                .feed
                .as_ref()
                .and_then(|feed| feed.sensor_by_handle(timeline.handle))
                .cloned()
            else {
                finished.push(index);
                continue;
            };
            loop {
                let Some(sample) = timeline.samples.get(timeline.next).cloned() else {
                    if timeline.looped {
                        timeline.next = 0;
                        timeline.cycle += 1;
                        let cycle_len = timeline
                            .samples
                            .last()
                            .map(|sample| sample.t_ms.max(0))
                            .unwrap_or(0)
                            + 1;
                        timeline.started_ns += cycle_len * 1_000_000;
                        continue;
                    }
                    finished.push(index);
                    break;
                };
                let due = timeline.started_ns + sample.t_ms.max(0) * 1_000_000;
                if due > now {
                    timeline_next = Some(
                        timeline_next.map_or(due, |current: i64| current.min(due)),
                    );
                    break;
                }
                due_events.push(SensorEvent::sample(&sensor, due, sample.values));
                if let Some(feed) = self.feed.as_mut() {
                    feed.values.insert(sensor.handle, sample.values);
                }
                timeline.next += 1;
            }
        }
        for index in finished.into_iter().rev() {
            self.timelines.remove(index);
        }
        for event in &due_events {
            self.deliver_to_enabled(event);
        }

        // 2. Periodic sampling: a continuous sensor streams at the period its
        //    connection asked for; an on-change sensor reports only when the
        //    value changed (as the HAL does).
        //
        //    A sensor with an active `play` timeline is **not** also sampled from
        //    its current value: the timeline *is* that sensor's hardware stream
        //    (a real HAL has one), so sampling it too would deliver two
        //    interleaved streams at different rates.
        let played: Vec<i32> = self.timelines.iter().map(|timeline| timeline.handle).collect();
        let mut next: Option<i64> = timeline_next;
        let mut due: Vec<(usize, SensorEvent)> = Vec::new();
        let connections: Vec<Arc<Mutex<ConnectionState>>> = self.connections.clone();
        for (index, connection) in connections.iter().enumerate() {
            let mut state = connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut handles: Vec<i32> = state.enabled.keys().copied().collect();
            handles.sort_unstable();
            for handle in handles {
                let period = *state.enabled.get(&handle).unwrap_or(&0);
                if period <= 0 || played.contains(&handle) {
                    continue;
                }
                let due_at = *state.next_due.entry(handle).or_insert(now + period);
                if due_at > now {
                    next = Some(next.map_or(due_at, |current: i64| current.min(due_at)));
                    continue;
                }
                let Some(sensor) = self
                    .feed
                    .as_ref()
                    .and_then(|feed| feed.sensor_by_handle(handle))
                    .cloned()
                else {
                    continue;
                };
                let Some(values) = self
                    .feed
                    .as_ref()
                    .and_then(|feed| feed.values.get(&handle).copied())
                else {
                    continue;
                };
                let changed = state.last.get(&handle).copied() != Some(values);
                // Advance on the period grid, never by "now + period", so a
                // late wake does not stretch the rate.
                let mut next_due = due_at + period;
                while next_due <= now {
                    next_due += period;
                }
                state.next_due.insert(handle, next_due);
                next = Some(next.map_or(next_due, |current: i64| current.min(next_due)));
                if sensor.is_continuous() || changed {
                    state.last.insert(handle, values);
                    due.push((index, SensorEvent::sample(&sensor, now, values)));
                }
            }
        }
        for (index, event) in due {
            let delivered = connections
                .get(index)
                .map(|connection| {
                    connection
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .deliver(&event)
                })
                .unwrap_or(false);
            if delivered {
                self.record(&event);
            } else {
                self.dropped += 1;
            }
        }
        next
    }

    /// The `sensors` control answer: the served table with current values.
    #[must_use]
    pub fn status_json(&self) -> Value {
        let Some(feed) = self.feed.as_ref() else {
            return json!({
                "attached": false,
                "sensors": [],
                "note": "no feed: `sensorservice` answers the stock device-without-sensors reply (empty list)",
            });
        };
        let enabled: Vec<Value> = self
            .connections
            .iter()
            .map(|connection| {
                let state = connection
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                json!({
                    "slot": state.slot,
                    "channel": state.has_channel(),
                    "enabled": state.enabled.iter().map(|(handle, period)| json!({
                        "handle": handle,
                        "period_ns": period,
                    })).collect::<Vec<_>>(),
                    "flush_calls": state.flush_calls,
                })
            })
            .collect();
        json!({
            "attached": true,
            "sensors": feed.sensors.iter().map(|sensor| {
                let values: Vec<f32> = feed
                    .values
                    .get(&sensor.handle)
                    .map(|values| values.to_vec())
                    .unwrap_or_default();
                json!({
                    "handle": sensor.handle,
                    "name": sensor.string_type,
                    "type": sensor.sensor_type,
                    "values": values,
                    "reporting": if sensor.is_continuous() { "continuous" } else { "on-change" },
                })
            }).collect::<Vec<_>>(),
            "connections": enabled,
            "delivered": self.delivered,
            "dropped": self.dropped,
            "set_value_calls": self.set_value_calls,
            "play_calls": self.play_calls,
            "timelines": self.timelines.len(),
        })
    }

    /// The recorded delivered events (last `limit`, newest last).
    #[must_use]
    pub fn events_json(&self, limit: usize) -> Value {
        let events: Vec<Value> = self.log.iter().rev().take(limit).rev().map(SensorEvent::to_json).collect();
        json!({
            "delivered": self.delivered,
            "dropped": self.dropped,
            "recorded": events.len(),
            "events": events,
        })
    }

    /// A one-line evidence sentence for the module report.
    #[must_use]
    pub fn summary(&self) -> String {
        match self.feed.as_ref() {
            None => "no feed: sensorservice answers the stock no-sensor reply".to_string(),
            Some(feed) => format!(
                "{} sensors ({}), {} events delivered, {} connections, {} play timelines{}",
                feed.sensors.len(),
                feed.sensors
                    .iter()
                    .map(|sensor| name_by_type(sensor.sensor_type))
                    .collect::<Vec<_>>()
                    .join("/"),
                self.delivered,
                self.connections.len(),
                self.timelines.len(),
                if self.dropped > 0 {
                    format!(", {} dropped (full tube)", self.dropped)
                } else {
                    String::new()
                }
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// The node
// ---------------------------------------------------------------------------

/// The condvar the writer thread sleeps on, shared with every connection so an
/// `enableDisable`/`setEventRate`/`flush`/`getSensorChannel` wakes the writer
/// immediately instead of waiting for the idle poll.
#[derive(Debug, Default)]
pub struct SensorWaker {
    wake: Condvar,
}

impl SensorWaker {
    /// Wake the writer thread.
    pub fn nudge(&self) {
        self.wake.notify_all();
    }
}

/// The writer thread and the hub it drains.
struct HubClock {
    waker: Arc<SensorWaker>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HubClock {
    fn start(shared: Arc<Mutex<SensorHub>>, waker: Arc<SensorWaker>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_shared = Arc::clone(&shared);
        let thread_waker = Arc::clone(&waker);
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("acos-sensors".into())
            .spawn(move || {
                let mut guard = thread_shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                while !thread_stop.load(Ordering::Relaxed) {
                    let now = now_ns();
                    let next = guard.tick(now);
                    let wait = match next {
                        // Wake on the grid, but never sleep longer than the
                        // shortest sampling period an app can ask for (1 ms).
                        Some(at) => Duration::from_nanos((at - now).clamp(0, 5_000_000) as u64),
                        // Idle: wait for a control op (`set_value`, `play`,
                        // `enable`) or the detach that clears the feed.
                        None => Duration::from_millis(50),
                    };
                    let (next_guard, _) = thread_waker
                        .wake
                        .wait_timeout(guard, wait)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    guard = next_guard;
                }
            })
            .ok();
        Self {
            waker,
            stop,
            thread,
        }
    }
}

impl Drop for HubClock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.waker.nudge();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One pool slot: the binder the host allocated plus the connection it serves.
#[derive(Debug)]
pub struct ConnectionSlot {
    binder: std::sync::atomic::AtomicU64,
    state: Mutex<Option<Arc<Mutex<ConnectionState>>>>,
}

impl ConnectionSlot {
    /// An unbound slot; [`install`] binds it right after `register_dynamic`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            binder: std::sync::atomic::AtomicU64::new(0),
            state: Mutex::new(None),
        }
    }

    fn bind(&self, binder: BinderId) {
        self.binder.store(binder.0, Ordering::Relaxed);
    }

    /// The binder this slot answers on.
    #[must_use]
    pub fn binder(&self) -> BinderId {
        BinderId(self.binder.load(Ordering::Relaxed))
    }

    fn install_state(&self, state: Arc<Mutex<ConnectionState>>) {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(state);
    }

    fn state(&self) -> Option<Arc<Mutex<ConnectionState>>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Default for ConnectionSlot {
    fn default() -> Self {
        Self::new()
    }
}

/// The `ISensorServer` node.
///
/// `core` publishes this node under the `sensorservice` name with no feed (the
/// stock device-without-sensors answer, and no libbinder retry loop); the
/// `sensors` module installs a feed into it while attached. The binder identity
/// never changes, so an app that resolved the service before a **hot** attach
/// keeps working.
/// The `ISensorServer` node.
///
/// `core` publishes this node under the `sensorservice` name with no feed (the
/// stock device-without-sensors answer, and no libbinder retry loop); the
/// `sensors` module installs a feed into it while attached. The binder identity
/// never changes, so an app that resolved the service before a **hot** attach
/// keeps working.
pub struct SensorServer {
    hub: Arc<Mutex<SensorHub>>,
    waker: Arc<SensorWaker>,
    clock: Option<HubClock>,
    slots: Vec<Arc<ConnectionSlot>>,
    /// `getSensorList`/`getDynamicSensorList` calls with their package argument.
    list_calls: Vec<String>,
    /// `createSensorEventConnection` calls, `(opPackage, mode)`.
    connection_calls: Vec<(String, i32)>,
}

/// How many `ISensorEventConnection` nodes a session can have open at once.
///
/// `SystemSensorManager` creates one `SensorEventQueue` per listener thread
/// (`BaseEventQueue`), so a handful is enough; a session that asks for more gets
/// a structured error rather than a silently shared tube (two queues reading one
/// `BitTube` would steal each other's events).
pub const CONNECTION_POOL_SIZE: usize = 8;

impl SensorServer {
    /// A node with no feed, no connections and an unbound slot pool (the node
    /// installs the pool's binders through [`install`]).
    #[must_use]
    pub fn new(hub: Arc<Mutex<SensorHub>>, waker: Arc<SensorWaker>) -> Self {
        Self {
            hub,
            waker,
            clock: None,
            slots: Vec::new(),
            list_calls: Vec::new(),
            connection_calls: Vec::new(),
        }
    }

    /// The hub (the module takes it at attach to drive the feed).
    #[must_use]
    pub fn hub(&self) -> Arc<Mutex<SensorHub>> {
        Arc::clone(&self.hub)
    }

    /// `true` when a feed is installed.
    #[must_use]
    pub fn has_feed(&self) -> bool {
        self.hub
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .has_feed()
    }

    /// Install a feed and start the writer thread (idempotent: a second install
    /// replaces the feed).
    pub fn install_feed(&mut self, feed: SensorFeed) {
        {
            let mut hub = self
                .hub
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            hub.install(feed);
        }
        if self.clock.is_none() {
            self.clock = Some(HubClock::start(
                Arc::clone(&self.hub),
                Arc::clone(&self.waker),
            ));
        } else {
            self.waker.nudge();
        }
    }

    /// Drop the feed and stop the writer thread (the module's `detach`).
    pub fn clear_feed(&mut self) {
        if let Some(clock) = self.clock.take() {
            // `HubClock::drop` stops and joins the thread *after* the feed is
            // gone, so no event can be written once detach returns.
            drop(clock);
        }
        self.hub
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    fn bad(method: &str, detail: impl std::fmt::Display) -> ServiceError {
        ServiceError::BadRequest {
            interface: SENSOR_SERVER_DESCRIPTOR.to_string(),
            method: method.to_string(),
            detail: detail.to_string(),
        }
    }

    fn write_sensor_list(&self, reply: &mut Parcel) -> SResult<()> {
        let sensors = {
            let hub = self
                .hub
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            hub.sensors().to_vec()
        };
        reply.write_int32(i32::try_from(sensors.len()).unwrap_or(i32::MAX));
        for sensor in &sensors {
            sensor.write_to_reply(reply)?;
        }
        Ok(())
    }

    fn open_connection(&mut self, method: &str) -> SResult<BinderId> {
        let Some(slot) = self
            .slots
            .iter()
            .enumerate()
            .find(|(_, slot)| {
                slot.state().is_none_or(|state| {
                    state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .destroyed
                })
            })
            .map(|(index, slot)| (index, Arc::clone(slot)))
        else {
            return Err(ServiceError::ResourceLimit {
                what: format!("{SENSOR_SERVER_DESCRIPTOR}::{method} event connections"),
                limit: self.slots.len(),
            });
        };
        let (index, slot) = slot;
        let state = {
            let mut hub = self
                .hub
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            hub.open_connection(index)
        };
        slot.install_state(state);
        self.waker.nudge();
        Ok(slot.binder())
    }
}

impl ServiceObject for SensorServer {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        _args: &[crate::value::Value],
    ) -> SResult<CallResult> {
        // `android.gui.*` has no decoded `acos-aidl` table: the native interface
        // is served entirely by `raw_transact` (as the compositor and
        // `IDisplayEventConnection` are).
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: method.name.clone(),
        })
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> SResult<bool> {
        if interface != SENSOR_SERVER_DESCRIPTOR {
            return Ok(false);
        }
        match code {
            server_codes::GET_SENSOR_LIST
            | server_codes::GET_DYNAMIC_SENSOR_LIST
            | server_codes::GET_RUNTIME_SENSOR_LIST => {
                // Hand-written proxy: no exception header, the reply starts with
                // `writeUint32(count)`.
                let package = data
                    .read_string16()
                    .map_err(|e| Self::bad("getSensorList", e))?
                    .unwrap_or_default();
                if code == server_codes::GET_RUNTIME_SENSOR_LIST {
                    let _device_id = data.read_int32().map_err(|e| Self::bad("getRuntimeSensorList", e))?;
                }
                self.list_calls.push(package);
                self.write_sensor_list(reply)?;
                Ok(true)
            }
            server_codes::CREATE_SENSOR_EVENT_CONNECTION => {
                let op_package = data
                    .read_string8()
                    .map_err(|e| Self::bad("createSensorEventConnection", e))?
                    .unwrap_or_default();
                let mode = data
                    .read_int32()
                    .map_err(|e| Self::bad("createSensorEventConnection", e))?;
                let _package = data
                    .read_string16()
                    .map_err(|e| Self::bad("createSensorEventConnection", e))?;
                let _attribution_tag = data
                    .read_string16()
                    .map_err(|e| Self::bad("createSensorEventConnection", e))?;
                self.connection_calls.push((op_package, mode));
                if !self.has_feed() {
                    // Stock device without sensors: the app can only ask for a
                    // queue after seeing a non-empty list, so the honest answer
                    // is the named gap (never a fabricated queue).
                    return Err(ServiceError::UnsupportedAndroidApi {
                        interface: SENSOR_SERVER_DESCRIPTOR.to_string(),
                        code,
                        method: "createSensorEventConnection (no sensor feed: attach the `sensors` \
                                 module)"
                            .to_string(),
                    });
                }
                let binder = self.open_connection("createSensorEventConnection")?;
                reply.write_strong_binder_opt(Some(binder));
                Ok(true)
            }
            server_codes::IS_DATA_INJECTION_ENABLED
            | server_codes::IS_REPLAY_DATA_INJECTION_ENABLED
            | server_codes::IS_HAL_BYPASS_REPLAY_DATA_INJECTION_ENABLED => {
                // `int32`: 0 = not enabled. ACOS injects only through the
                // scripted feed, which is not the data-injection HAL path.
                reply.write_int32(0);
                Ok(true)
            }
            server_codes::CREATE_SENSOR_DIRECT_CONNECTION => {
                // A direct channel is a shared-memory `NativeHandle` the app
                // allocates; ACOS has no HAL to map it into.
                Err(ServiceError::UnsupportedAndroidApi {
                    interface: SENSOR_SERVER_DESCRIPTOR.to_string(),
                    code,
                    method: "createSensorDirectConnection (no sensor HAL / shared memory channel)"
                        .to_string(),
                })
            }
            server_codes::SET_OPERATION_PARAMETER => Err(ServiceError::UnsupportedAndroidApi {
                interface: SENSOR_SERVER_DESCRIPTOR.to_string(),
                code,
                method: "setOperationParameter (no vendor sensor operation)".to_string(),
            }),
            _ => Ok(false),
        }
    }
}

/// The `ISensorEventConnection` node for one pool slot.
pub struct SensorEventConnectionService {
    slot: Arc<ConnectionSlot>,
    hub: Arc<Mutex<SensorHub>>,
    waker: Arc<SensorWaker>,
}

impl SensorEventConnectionService {
    /// Serve `slot` out of `hub`.
    #[must_use]
    pub fn new(
        slot: Arc<ConnectionSlot>,
        hub: Arc<Mutex<SensorHub>>,
        waker: Arc<SensorWaker>,
    ) -> Self {
        Self { slot, hub, waker }
    }

    fn state(&self) -> Option<Arc<Mutex<ConnectionState>>> {
        self.slot.state()
    }
}

impl ServiceObject for SensorEventConnectionService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        _args: &[crate::value::Value],
    ) -> SResult<CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: method.name.clone(),
        })
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> SResult<bool> {
        if interface != SENSOR_EVENT_CONNECTION_DESCRIPTOR {
            return Ok(false);
        }
        let bad = |method: &str, detail: String| ServiceError::BadRequest {
            interface: SENSOR_EVENT_CONNECTION_DESCRIPTOR.to_string(),
            method: method.to_string(),
            detail,
        };
        let Some(state) = self.state() else {
            return Err(bad(
                "code",
                format!("code {code} on a connection slot that was never opened"),
            ));
        };
        match code {
            connection_codes::GET_SENSOR_CHANNEL => {
                // `gui::BitTube::writeToParcel` for a hand-written interface:
                // receive fd, then send fd, and nothing else (the proxy dups them
                // with two bare `readFileDescriptor` calls).
                let mut tube = crate::display_events::BitTube::new().map_err(|e| {
                    ServiceError::Internal(format!("ISensorEventConnection: socketpair: {e}"))
                })?;
                let mut guard = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                tube.write_to_parcel_raw(reply)?;
                guard.tube = Some(tube);
                drop(guard);
                // A channel that appears after `enableDisable` must start
                // sampling now, not at the next idle poll.
                self.nudge();
                Ok(true)
            }
            connection_codes::ENABLE_DISABLE => {
                let handle = data.read_int32().map_err(|e| bad("enableDisable", e.to_string()))?;
                let enabled = data
                    .read_int32()
                    .map_err(|e| bad("enableDisable", e.to_string()))?;
                let sampling_period_ns = data
                    .read_int64()
                    .map_err(|e| bad("enableDisable", e.to_string()))?;
                let _max_batch_ns = data
                    .read_int64()
                    .map_err(|e| bad("enableDisable", e.to_string()))?;
                let _reserved_flags = data
                    .read_int32()
                    .map_err(|e| bad("enableDisable", e.to_string()))?;
                let known = {
                    let hub = self
                        .hub
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    hub.sensors().iter().any(|sensor| sensor.handle == handle)
                };
                let status = if !known {
                    BAD_VALUE
                } else {
                    let mut guard = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    guard.enable_log.push((handle, enabled != 0, sampling_period_ns));
                    if enabled != 0 {
                        let period = sampling_period_ns.max(1_000_000);
                        guard.enabled.insert(handle, period);
                        guard.next_due.insert(handle, now_ns() + period);
                    } else {
                        guard.enabled.remove(&handle);
                        guard.next_due.remove(&handle);
                        guard.last.remove(&handle);
                    }
                    NO_ERROR
                };
                reply.write_int32(status);
                self.nudge();
                Ok(true)
            }
            connection_codes::SET_EVENT_RATE => {
                let handle = data.read_int32().map_err(|e| bad("setEventRate", e.to_string()))?;
                let ns = data.read_int64().map_err(|e| bad("setEventRate", e.to_string()))?;
                let mut guard = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let status = if guard.enabled.contains_key(&handle) {
                    let period = ns.max(1_000_000);
                    guard.enabled.insert(handle, period);
                    guard.next_due.insert(handle, now_ns() + period);
                    NO_ERROR
                } else {
                    BAD_VALUE
                };
                drop(guard);
                reply.write_int32(status);
                self.nudge();
                Ok(true)
            }
            connection_codes::FLUSH_SENSOR => {
                // `SensorService` answers `flush()` with one
                // `META_DATA_FLUSH_COMPLETE` event per enabled sensor on this
                // connection, then `NO_ERROR`. The events are recorded like any
                // other delivered event, so a verdict can cite them.
                let now = now_ns();
                let sensors = {
                    let hub = self
                        .hub
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    hub.sensors().to_vec()
                };
                let enabled: Vec<i32> = {
                    let mut guard = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    guard.flush_calls += 1;
                    guard.enabled.keys().copied().collect()
                };
                let mut hub = self
                    .hub
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for handle in enabled {
                    if let Some(sensor) = sensors.iter().find(|sensor| sensor.handle == handle) {
                        let event = SensorEvent::flush_complete(sensor, now);
                        hub.deliver_to(&state, &event);
                    }
                }
                drop(hub);
                reply.write_int32(NO_ERROR);
                Ok(true)
            }
            connection_codes::CONFIGURE_CHANNEL => {
                let _handle = data
                    .read_int32()
                    .map_err(|e| bad("configureChannel", e.to_string()))?;
                let _rate_level = data
                    .read_int32()
                    .map_err(|e| bad("configureChannel", e.to_string()))?;
                reply.write_int32(NO_ERROR);
                Ok(true)
            }
            connection_codes::DESTROY => {
                // `BpSensorEventConnection::destroy` transacts without reading a
                // reply; the connection stops and its tube closes.
                let mut guard = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard.destroyed = true;
                guard.enabled.clear();
                guard.tube = None;
                drop(guard);
                self.nudge();
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

impl SensorEventConnectionService {
    /// Wake the writer thread (`SensorWaker::nudge`).
    fn nudge(&self) {
        self.waker.nudge();
    }
}

/// Register the `sensorservice` node and its connection pool on `host`.
///
/// The node is published with **no** feed: the stock answer of a device without
/// sensors (an empty list). A module installs a feed into the same node, so the
/// binder identity an app already holds stays valid across a hot attach.
pub fn install(host: &mut ServiceHost, hub: Arc<Mutex<SensorHub>>) -> Arc<Mutex<SensorHub>> {
    install_with_pool(host, hub, CONNECTION_POOL_SIZE)
}

/// One installed sensor service: the shared hub (the module's handle to the
/// feed) and the writer's waker.
#[derive(Clone, Debug)]
pub struct Installed {
    /// The hub the node and every connection share.
    pub hub: Arc<Mutex<SensorHub>>,
    /// The writer thread's wake handle.
    pub waker: Arc<SensorWaker>,
}

/// [`install`] with an explicit connection-pool size (tests).
pub fn install_with_pool(
    host: &mut ServiceHost,
    hub: Arc<Mutex<SensorHub>>,
    pool: usize,
) -> Arc<Mutex<SensorHub>> {
    install_full(host, hub, pool).hub
}

/// [`install_with_pool`], also returning the writer's waker.
pub fn install_full(
    host: &mut ServiceHost,
    hub: Arc<Mutex<SensorHub>>,
    pool: usize,
) -> Installed {
    let waker = Arc::new(SensorWaker::default());
    let mut slots = Vec::with_capacity(pool);
    for _ in 0..pool {
        let slot = Arc::new(ConnectionSlot::new());
        let binder = host.register_dynamic(
            SENSOR_EVENT_CONNECTION_DESCRIPTOR,
            Box::new(SensorEventConnectionService::new(
                Arc::clone(&slot),
                Arc::clone(&hub),
                Arc::clone(&waker),
            )),
        );
        slot.bind(binder);
        slots.push(slot);
    }
    let mut server = SensorServer::new(Arc::clone(&hub), Arc::clone(&waker));
    server.slots = slots;
    // `sensorservice` is reached through `ServiceManager`, so the node is
    // published by name. It has no `acos-aidl` table: `raw_transact` serves it.
    host.register_dynamic_named(
        SERVICE_NAME,
        SENSOR_SERVER_DESCRIPTOR,
        Box::new(server),
    );
    Installed { hub, waker }
}
