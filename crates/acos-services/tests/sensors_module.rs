//! MODSENS: the `sensors` module against the **native** wire.
//!
//! Every assertion here reads the reply the way the framework's own proxy reads
//! it — `libsensor`'s `BpSensorServer`/`BpSensorEventConnection`, which are
//! hand-written C++ and therefore expect **no** `binder::Status` header:
//!
//! * `getSensorList` → `uint32 count`, then per sensor `int32 flattenedSize` +
//!   `Sensor::flatten` bytes (`libsensor/Sensor.cpp:534`).
//! * `getSensorChannel` → `readFileDescriptor` twice (receive, send).
//! * `enableDisable`/`setEventRate`/`flush` → one `status_t` int32.
//! * events → 104-byte `ASensorEvent` records on the tube
//!   (`SensorEventQueue::read`'s `objSize = 0x68` in the image).
//!
//! The feed drives real `send(2)` messages through a real `SOCK_SEQPACKET`
//! `BitTube`, so these tests read the same bytes the guest's `SensorEventQueue`
//! would.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use acos_parcel::{BinderId, Parcel};
use acos_services::module::{self, ModuleConfig, ModuleRegistry, Profile};
use acos_services::sensors::{
    self, EVENT_OFFSET_DATA, EVENT_OFFSET_HANDLE, EVENT_OFFSET_TIMESTAMP, EVENT_OFFSET_TYPE,
    EVENT_SIZE, TYPE_ACCELEROMETER, TYPE_LIGHT, TYPE_META_DATA, TYPE_PROXIMITY,
};
use acos_services::{ServiceError, ServiceHost};

/// A host with `core` attached (a real session: `core` publishes the
/// `sensorservice` node) and nothing else.
fn session() -> (ServiceHost, ModuleRegistry) {
    let mut host = ServiceHost::new();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, "core", Profile::Real, &ModuleConfig::default())
        .expect("core attaches");
    (host, registry)
}

fn transact(
    host: &mut ServiceHost,
    binder: BinderId,
    code: u32,
    data: &mut Parcel,
) -> Result<Parcel, ServiceError> {
    let mut reply = Parcel::new();
    host.transact(binder, code, data, &mut reply, 0)?;
    reply.set_data_position(0);
    Ok(reply)
}

fn sensor_list_request(package: &str) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(sensors::SENSOR_SERVER_DESCRIPTOR)
        .expect("token");
    data.write_string16(package).expect("package");
    data.set_data_position(0);
    data
}

fn create_connection_request(mode: i32) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(sensors::SENSOR_SERVER_DESCRIPTOR)
        .expect("token");
    data.write_string8("com.acos.sensorprobe").expect("string8");
    data.write_int32(mode);
    data.write_string16("com.acos.sensorprobe")
        .expect("package");
    data.write_string16("").expect("attribution");
    data.set_data_position(0);
    data
}

fn connection_request(_code: u32, body: impl FnOnce(&mut Parcel)) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(sensors::SENSOR_EVENT_CONNECTION_DESCRIPTOR)
        .expect("token");
    body(&mut data);
    data.set_data_position(0);
    data
}

/// `Sensor::unflatten`: read one flattened sensor out of `bytes`.
struct SensorReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> SensorReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn i32(&mut self) -> i32 {
        let mut word = [0u8; 4];
        word.copy_from_slice(&self.bytes[self.pos..self.pos + 4]);
        self.pos += 4;
        i32::from_le_bytes(word)
    }

    fn u32(&mut self) -> u32 {
        self.i32() as u32
    }

    fn f32(&mut self) -> f32 {
        f32::from_bits(self.i32() as u32)
    }

    fn bool1(&mut self) -> bool {
        let byte = self.bytes[self.pos];
        self.pos += 1;
        byte != 0
    }

    /// `Sensor::unflattenString8`: `u32 length` + bytes + pad to 4.
    fn string8(&mut self) -> String {
        let len = self.u32() as usize;
        let text = String::from_utf8_lossy(&self.bytes[self.pos..self.pos + len]).to_string();
        self.pos += len;
        self.pos += (4 - (len % 4)) % 4;
        text
    }
}

/// One decoded `Sensor`.
#[derive(Debug, PartialEq)]
struct DecodedSensor {
    name: String,
    vendor: String,
    version: i32,
    handle: i32,
    sensor_type: i32,
    min_value: f32,
    max_value: f32,
    resolution: f32,
    power: f32,
    min_delay: i32,
    fifo_reserved: u32,
    fifo_max: u32,
    string_type: String,
    required_permission: String,
    required_permission_runtime: bool,
    required_app_op: i32,
    max_delay: i32,
    flags: u32,
    uuid: [u8; 16],
    id: i32,
}

fn decode_sensor(bytes: &[u8]) -> DecodedSensor {
    let mut r = SensorReader::new(bytes);
    let name = r.string8();
    let vendor = r.string8();
    let version = r.i32();
    let handle = r.i32();
    let sensor_type = r.i32();
    let min_value = r.f32();
    let max_value = r.f32();
    let resolution = r.f32();
    let power = r.f32();
    let min_delay = r.i32();
    let fifo_reserved = r.u32();
    let fifo_max = r.u32();
    let string_type = r.string8();
    let required_permission = r.string8();
    let required_permission_runtime = r.bool1();
    let required_app_op = r.i32();
    let max_delay = r.i32();
    let flags = r.u32();
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&r.bytes[r.pos..r.pos + 16]);
    r.pos += 16;
    let id = r.i32();
    assert_eq!(
        r.pos,
        bytes.len(),
        "the flattened Sensor is exactly one payload"
    );
    DecodedSensor {
        name,
        vendor,
        version,
        handle,
        sensor_type,
        min_value,
        max_value,
        resolution,
        power,
        min_delay,
        fifo_reserved,
        fifo_max,
        string_type,
        required_permission,
        required_permission_runtime,
        required_app_op,
        max_delay,
        flags,
        uuid,
        id,
    }
}

/// Read `getSensorList`'s reply the way `BpSensorServer` does.
fn read_sensor_list(reply: &mut Parcel) -> Vec<DecodedSensor> {
    let count = reply.read_uint32().expect("count") as usize;
    let mut sensors = Vec::with_capacity(count);
    for _ in 0..count {
        let size = reply.read_int32().expect("flattenedSize") as usize;
        let bytes = reply.read_raw_bytes(size).expect("flattened Sensor");
        // `Parcel::writeInplace` pads the element to four bytes
        // (`pad_size(len)`), and the client advances by the padded size.
        let pad = (4 - (size % 4)) % 4;
        let padding = reply.read_raw_bytes(pad).expect("element padding");
        assert!(padding.iter().all(|byte| *byte == 0), "zero padding");
        sensors.push(decode_sensor(&bytes));
    }
    assert_eq!(reply.data_avail(), 0, "the reply is exactly the list");
    sensors
}

fn open_connection(host: &mut ServiceHost, server: BinderId) -> BinderId {
    let mut data = create_connection_request(0);
    let mut reply = transact(
        host,
        server,
        sensors::server_codes::CREATE_SENSOR_EVENT_CONNECTION,
        &mut data,
    )
    .expect("createSensorEventConnection");
    reply
        .read_strong_binder()
        .expect("binder")
        .expect("non-null ISensorEventConnection")
}

/// `getSensorChannel`: the two descriptors, and nothing else in front of them.
fn take_channel(host: &mut ServiceHost, connection: BinderId) -> (OwnedFd, OwnedFd) {
    let mut data = connection_request(sensors::connection_codes::GET_SENSOR_CHANNEL, |_| {});
    let mut reply = transact(
        host,
        connection,
        sensors::connection_codes::GET_SENSOR_CHANNEL,
        &mut data,
    )
    .expect("getSensorChannel");
    let receive = reply
        .read_file_descriptor()
        .expect("receive fd")
        .expect("non-null");
    let send = reply
        .read_file_descriptor()
        .expect("send fd")
        .expect("non-null");
    assert_eq!(
        reply.data_avail(),
        0,
        "a hand-written BitTube reply is two descriptors and nothing else"
    );
    // The numbers are owned by the parcel, which is dropped here; duplicate them
    // so the test can read from the receive end.
    let receive =
        rustix::io::dup(reply.borrowed_file_descriptor(receive).expect("borrow")).expect("dup");
    let send = rustix::io::dup(reply.borrowed_file_descriptor(send).expect("borrow")).expect("dup");
    (receive, send)
}

fn enable(
    host: &mut ServiceHost,
    connection: BinderId,
    handle: i32,
    enabled: bool,
    period_ns: i64,
) -> i32 {
    let mut data = connection_request(sensors::connection_codes::ENABLE_DISABLE, |data| {
        data.write_int32(handle);
        data.write_int32(i32::from(enabled));
        data.write_int64(period_ns);
        data.write_int64(0);
        data.write_int32(0);
    });
    let mut reply = transact(
        host,
        connection,
        sensors::connection_codes::ENABLE_DISABLE,
        &mut data,
    )
    .expect("enableDisable");
    let status = reply.read_int32().expect("status_t");
    assert_eq!(reply.data_avail(), 0);
    status
}

/// Receive one 104-byte event from the tube (as `BitTube::recvObjects` does).
fn recv_event(fd: BorrowedFd<'_>, timeout: Duration) -> Option<[u8; EVENT_SIZE]> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut buf = [0u8; EVENT_SIZE];
        match rustix::net::recv(fd, &mut buf, rustix::net::RecvFlags::DONTWAIT) {
            Ok((len, _flags)) => {
                assert_eq!(len, EVENT_SIZE, "one send(2) message is one event");
                return Some(buf);
            }
            Err(_) => {
                if Instant::now() > deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

fn event_handle(event: &[u8; EVENT_SIZE]) -> i32 {
    i32::from_le_bytes(
        event[EVENT_OFFSET_HANDLE..EVENT_OFFSET_HANDLE + 4]
            .try_into()
            .unwrap(),
    )
}

fn event_type(event: &[u8; EVENT_SIZE]) -> i32 {
    i32::from_le_bytes(
        event[EVENT_OFFSET_TYPE..EVENT_OFFSET_TYPE + 4]
            .try_into()
            .unwrap(),
    )
}

fn event_timestamp(event: &[u8; EVENT_SIZE]) -> i64 {
    i64::from_le_bytes(
        event[EVENT_OFFSET_TIMESTAMP..EVENT_OFFSET_TIMESTAMP + 8]
            .try_into()
            .unwrap(),
    )
}

fn event_values(event: &[u8; EVENT_SIZE]) -> [f32; 16] {
    let mut values = [0.0f32; 16];
    for (index, value) in values.iter_mut().enumerate() {
        let at = EVENT_OFFSET_DATA + index * 4;
        *value = f32::from_le_bytes(event[at..at + 4].try_into().unwrap());
    }
    values
}

// ---------------------------------------------------------------------------
// The wire
// ---------------------------------------------------------------------------

/// `ASensorEvent`'s layout is the image's, not a guess: 104 bytes
/// (`SensorEventQueue::read`'s `mov $0x68` in `libsensor.so`) with the handle at
/// 4, the type at 8, the timestamp at 16 and `data[16]` at 24.
#[test]
fn the_event_packet_is_the_image_s_asensor_event() {
    assert_eq!(EVENT_SIZE, 0x68);
    let sensor = sensors::default_sensors()
        .into_iter()
        .find(|sensor| sensor.sensor_type == TYPE_ACCELEROMETER)
        .expect("accelerometer");
    let mut values = [0.0f32; 16];
    values[0] = 1.5;
    values[1] = -2.5;
    values[2] = 9.81;
    let event = sensors::SensorEvent::sample(&sensor, 1_234_567_890, values);
    let bytes = event.encode();
    assert_eq!(bytes.len(), 104);
    assert_eq!(
        i32::from_le_bytes(bytes[0..4].try_into().unwrap()),
        104,
        "version is sizeof(ASensorEvent)"
    );
    assert_eq!(event_handle(&bytes), sensor.handle);
    assert_eq!(event_type(&bytes), TYPE_ACCELEROMETER);
    assert_eq!(event_timestamp(&bytes), 1_234_567_890);
    assert_eq!(&event_values(&bytes)[0..3], &[1.5, -2.5, 9.81]);
}

/// A step counter event carries its count in the `u64.step_counter` union
/// member, which is what `android_hardware_SensorManager.cpp` reads for
/// `SENSOR_TYPE_STEP_COUNTER` (floats would be a different number).
#[test]
fn a_step_counter_event_carries_a_u64_not_a_float() {
    let step = sensors::default_sensors()
        .into_iter()
        .find(|sensor| sensor.sensor_type == sensors::TYPE_STEP_COUNTER)
        .expect("step counter");
    let mut values = [0.0f32; 16];
    values[0] = 1_234_567.0;
    let bytes = sensors::SensorEvent::sample(&step, 1, values).encode();
    assert_eq!(
        u64::from_le_bytes(
            bytes[EVENT_OFFSET_DATA..EVENT_OFFSET_DATA + 8]
                .try_into()
                .unwrap()
        ),
        1_234_567
    );
}

// ---------------------------------------------------------------------------
// The off state: `core`'s node, no feed
// ---------------------------------------------------------------------------

/// With the module off, `sensorservice` answers exactly like a device without
/// sensors: an empty list (no exception header in front of the count), and a
/// named gap for the event channel.
#[test]
fn the_off_state_is_the_stock_device_without_sensors() {
    let (mut host, _registry) = session();
    let server = host
        .check_service("sensorservice")
        .expect("lookup")
        .expect("published");

    let mut data = sensor_list_request("com.acos.sensorprobe");
    let mut reply = transact(
        &mut host,
        server,
        sensors::server_codes::GET_SENSOR_LIST,
        &mut data,
    )
    .expect("getSensorList");
    assert_eq!(reply.read_int32().expect("count"), 0);
    assert_eq!(reply.data_avail(), 0);

    let mut data = create_connection_request(0);
    let error = transact(
        &mut host,
        server,
        sensors::server_codes::CREATE_SENSOR_EVENT_CONNECTION,
        &mut data,
    )
    .expect_err("no fabricated queue without a feed");
    assert_eq!(error.code(), "UNSUPPORTED_ANDROID_API", "{error}");

    // `isDataInjectionEnabled()` answers 0, not a named gap: the code is known.
    let mut data = Parcel::new();
    data.write_interface_token(sensors::SENSOR_SERVER_DESCRIPTOR)
        .expect("token");
    data.set_data_position(0);
    let mut reply = transact(
        &mut host,
        server,
        sensors::server_codes::IS_DATA_INJECTION_ENABLED,
        &mut data,
    )
    .expect("isDataInjectionEnabled");
    assert_eq!(reply.read_int32().expect("int"), 0);
}

// ---------------------------------------------------------------------------
// The scripted feed
// ---------------------------------------------------------------------------

/// The list is the seven framework sensors, flattened exactly as
/// `Sensor::flatten` writes them, and the app's own reader sees the fields it
/// would see on a phone.
#[test]
fn the_scripted_list_is_the_framework_sensor_table() {
    let (mut host, mut registry) = session();
    registry
        .attach(
            &mut host,
            "sensors",
            Profile::Scripted,
            &ModuleConfig::default(),
        )
        .expect("attach sensors:scripted");
    let server = host
        .check_service("sensorservice")
        .expect("lookup")
        .expect("published");
    let mut data = sensor_list_request("com.acos.sensorprobe");
    let mut reply = transact(
        &mut host,
        server,
        sensors::server_codes::GET_SENSOR_LIST,
        &mut data,
    )
    .expect("getSensorList");
    let list = read_sensor_list(&mut reply);

    let types: Vec<i32> = list.iter().map(|sensor| sensor.sensor_type).collect();
    assert_eq!(
        types,
        vec![
            TYPE_ACCELEROMETER,
            sensors::TYPE_MAGNETIC_FIELD,
            sensors::TYPE_GYROSCOPE,
            TYPE_LIGHT,
            TYPE_PROXIMITY,
            sensors::TYPE_ROTATION_VECTOR,
            sensors::TYPE_STEP_COUNTER,
        ]
    );
    let accelerometer = &list[0];
    assert_eq!(accelerometer.string_type, "android.sensor.accelerometer");
    assert_eq!(accelerometer.name, "BMI160 Accelerometer");
    assert_eq!(accelerometer.vendor, "Bosch");
    assert!(accelerometer.max_value >= 19.6, "range must fit 2 g");
    assert!(accelerometer.resolution > 0.0);
    assert_eq!(accelerometer.min_delay, 5_000);
    assert_eq!(
        accelerometer.flags & sensors::FLAG_ON_CHANGE_MODE,
        0,
        "continuous"
    );
    assert_eq!(accelerometer.uuid, [0u8; 16]);
    assert_eq!(accelerometer.required_permission_runtime, false);
    assert_eq!(accelerometer.required_app_op, -1);

    let light = list
        .iter()
        .find(|s| s.sensor_type == TYPE_LIGHT)
        .expect("light");
    assert_eq!(
        light.flags & sensors::FLAG_ON_CHANGE_MODE,
        sensors::FLAG_ON_CHANGE_MODE,
        "light reports on change, as a phone does"
    );
    let step = list
        .iter()
        .find(|s| s.sensor_type == sensors::TYPE_STEP_COUNTER)
        .expect("step");
    assert_eq!(
        step.required_permission,
        "android.permission.ACTIVITY_RECOGNITION"
    );
    let handles: Vec<i32> = list.iter().map(|sensor| sensor.handle).collect();
    let mut sorted = handles.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), handles.len(), "handles are unique");
}

/// The whole chain an app walks: list → connection → channel → enable → events,
/// with the events arriving at the rate the app asked for.
#[test]
fn a_scripted_value_arrives_over_the_real_event_channel_at_the_requested_rate() {
    let (mut host, mut registry) = session();
    registry
        .attach(
            &mut host,
            "sensors",
            Profile::Scripted,
            &ModuleConfig::default(),
        )
        .expect("attach");
    let server = host
        .check_service("sensorservice")
        .expect("lookup")
        .expect("binder");
    let connection = open_connection(&mut host, server);
    let (receive, _send) = take_channel(&mut host, connection);

    // 20 ms period = 50 Hz, the rate `registerListener(..., SENSOR_DELAY_GAME)`
    // asks for.
    assert_eq!(enable(&mut host, connection, 1, true, 20_000_000), 0);
    registry
        .control(
            &mut host,
            "sensors",
            "set_value",
            &serde_json::json!({"sensor": "accelerometer", "values": [1.5, -2.0, 9.81]}),
        )
        .expect("set_value");

    let receive = receive.as_fd();
    let first = recv_event(receive, Duration::from_millis(500)).expect("one event");
    assert_eq!(event_handle(&first), 1);
    assert_eq!(event_type(&first), TYPE_ACCELEROMETER);
    let values = event_values(&first);
    assert_eq!(&values[0..3], &[1.5, -2.0, 9.81]);

    // The rate: consecutive timestamps are one period apart (+/- a tick of
    // scheduling jitter, never a multiple of it).
    let second = recv_event(receive, Duration::from_millis(500)).expect("second event");
    let delta = event_timestamp(&second) - event_timestamp(&first);
    assert!(
        (10_000_000..=30_000_000).contains(&delta),
        "20 ms period, got {delta} ns"
    );

    let status = registry
        .control(&mut host, "sensors", "status", &serde_json::json!({}))
        .expect("status");
    assert_eq!(status["attached"], true);
    assert!(status["delivered"].as_u64().unwrap_or(0) >= 2);
    assert_eq!(status["connections"][0]["enabled"][0]["handle"], 1);
}

/// `set_value` on an on-change sensor (light) is delivered immediately and then
/// only when the value changes — the HAL behaviour, not a free-running stream.
#[test]
fn an_on_change_sensor_reports_on_change_only() {
    let (mut host, mut registry) = session();
    registry
        .attach(
            &mut host,
            "sensors",
            Profile::Scripted,
            &ModuleConfig::default(),
        )
        .expect("attach");
    let server = host
        .check_service("sensorservice")
        .expect("lookup")
        .expect("binder");
    let connection = open_connection(&mut host, server);
    let (receive, _send) = take_channel(&mut host, connection);
    // The light's handle is 4 in the default table.
    let handle = registry
        .control(&mut host, "sensors", "status", &serde_json::json!({}))
        .expect("status")
        .pointer("/sensors")
        .and_then(|sensors| sensors.as_array())
        .and_then(|sensors| {
            sensors
                .iter()
                .find(|sensor| sensor["type"] == TYPE_LIGHT)
                .and_then(|sensor| sensor["handle"].as_i64())
        })
        .expect("light handle") as i32;
    assert_eq!(enable(&mut host, connection, handle, true, 10_000_000), 0);

    let receive = receive.as_fd();
    // The first sample is the default value (100 lux).
    let first = recv_event(receive, Duration::from_millis(500)).expect("first light event");
    assert_eq!(event_values(&first)[0], 100.0);

    // No change: no further events for 5 periods.
    assert!(
        recv_event(receive, Duration::from_millis(60)).is_none(),
        "an unchanged on-change sensor must not stream"
    );

    // A change is delivered at once (the `set_value` path), not one period late.
    registry
        .control(
            &mut host,
            "sensors",
            "set_value",
            &serde_json::json!({"sensor": "light", "values": [1234.0]}),
        )
        .expect("set_value");
    let changed = recv_event(receive, Duration::from_millis(200)).expect("changed light event");
    assert_eq!(event_values(&changed)[0], 1234.0);
}

/// `play` delivers the timeline's samples as events, in order, at their own
/// offsets — this is what the shake helper builds.
#[test]
fn play_delivers_the_timeline_and_shake_alternates() {
    let (mut host, mut registry) = session();
    registry
        .attach(
            &mut host,
            "sensors",
            Profile::Scripted,
            &ModuleConfig::default(),
        )
        .expect("attach");
    let server = host
        .check_service("sensorservice")
        .expect("lookup")
        .expect("binder");
    let connection = open_connection(&mut host, server);
    let (receive, _send) = take_channel(&mut host, connection);
    assert_eq!(enable(&mut host, connection, 1, true, 100_000_000), 0);

    let reply = registry
        .control(
            &mut host,
            "sensors",
            "play",
            &serde_json::json!({
                "sensor": "accelerometer",
                "samples": [
                    {"t_ms": 0, "values": [2.0, 0.0, 9.81]},
                    {"t_ms": 20, "values": [-2.0, 0.0, 9.81]},
                    {"t_ms": 40, "values": [0.0, 2.0, 9.81]},
                ],
            }),
        )
        .expect("play");
    assert_eq!(reply["scheduled"], 3);
    assert_eq!(reply["duration_ms"], 40);

    let receive = receive.as_fd();
    let mut seen = Vec::new();
    for _ in 0..3 {
        let event = recv_event(receive, Duration::from_millis(500)).expect("timeline event");
        seen.push(event_values(&event)[0]);
    }
    assert_eq!(seen, vec![2.0, -2.0, 0.0]);

    // The `shake` helper is a 12-sample alternating accelerometer timeline.
    let reply = registry
        .control(
            &mut host,
            "sensors",
            "shake",
            &serde_json::json!({"period_ms": 10, "count": 12}),
        )
        .expect("shake");
    assert_eq!(reply["scheduled"], 12);
    let mut signs = Vec::new();
    for _ in 0..12 {
        let event = recv_event(receive, Duration::from_millis(500)).expect("shake event");
        signs.push(if event_values(&event)[0] > 0.0 { 1 } else { -1 });
    }
    assert_eq!(
        signs,
        vec![1, -1, 1, -1, 1, -1, 1, -1, 1, -1, 1, -1],
        "a shake alternates around gravity"
    );
}

/// `flush()` answers `NO_ERROR` **and** delivers one `META_DATA_FLUSH_COMPLETE`
/// per enabled sensor, which is how `onFlushCompleted` fires.
#[test]
fn flush_delivers_the_flush_complete_meta_event() {
    let (mut host, mut registry) = session();
    registry
        .attach(
            &mut host,
            "sensors",
            Profile::Scripted,
            &ModuleConfig::default(),
        )
        .expect("attach");
    let server = host
        .check_service("sensorservice")
        .expect("lookup")
        .expect("binder");
    let connection = open_connection(&mut host, server);
    let (receive, _send) = take_channel(&mut host, connection);
    assert_eq!(enable(&mut host, connection, 1, true, 1_000_000_000), 0);

    let mut data = connection_request(sensors::connection_codes::FLUSH_SENSOR, |_| {});
    let mut reply = transact(
        &mut host,
        connection,
        sensors::connection_codes::FLUSH_SENSOR,
        &mut data,
    )
    .expect("flush");
    assert_eq!(reply.read_int32().expect("status"), sensors::NO_ERROR);

    let receive = receive.as_fd();
    let mut flush_event = None;
    for _ in 0..8 {
        let Some(event) = recv_event(receive, Duration::from_millis(300)) else {
            break;
        };
        if event_type(&event) == TYPE_META_DATA {
            flush_event = Some(event);
            break;
        }
    }
    let flush_event = flush_event.expect("a META_DATA_FLUSH_COMPLETE event");
    let values = event_values(&flush_event);
    assert_eq!(values[0], sensors::META_DATA_FLUSH_COMPLETE as f32);
    assert_eq!(values[1], 1.0, "meta_data.sensor is the handle");

    let events = registry
        .control(
            &mut host,
            "sensors",
            "events",
            &serde_json::json!({"limit": 8}),
        )
        .expect("events");
    assert!(
        events["events"]
            .as_array()
            .expect("events array")
            .iter()
            .any(|event| event["flush_complete"] == true),
        "the flush-complete is recorded as delivered evidence: {events}"
    );
}

/// `disable` stops the stream; `destroy` ends the connection.
#[test]
fn disabling_stops_the_stream_and_destroy_ends_the_connection() {
    let (mut host, mut registry) = session();
    registry
        .attach(
            &mut host,
            "sensors",
            Profile::Scripted,
            &ModuleConfig::default(),
        )
        .expect("attach");
    let server = host
        .check_service("sensorservice")
        .expect("lookup")
        .expect("binder");
    let connection = open_connection(&mut host, server);
    let (receive, _send) = take_channel(&mut host, connection);
    assert_eq!(enable(&mut host, connection, 1, true, 5_000_000), 0);
    let receive_borrowed = receive.as_fd();
    recv_event(receive_borrowed, Duration::from_millis(500)).expect("streaming");

    assert_eq!(enable(&mut host, connection, 1, false, 5_000_000), 0);
    // Drain whatever was already in the tube (the socket is 4 KiB = ~39
    // events), then require silence.
    while recv_event(receive_borrowed, Duration::from_millis(20)).is_some() {}
    assert!(
        recv_event(receive_borrowed, Duration::from_millis(100)).is_none(),
        "a disabled sensor must not deliver"
    );

    let mut data = connection_request(sensors::connection_codes::DESTROY, |_| {});
    transact(
        &mut host,
        connection,
        sensors::connection_codes::DESTROY,
        &mut data,
    )
    .expect("destroy");
    // A request after destroy is still a structured answer, never a panic.
    let mut data = connection_request(sensors::connection_codes::SET_EVENT_RATE, |data| {
        data.write_int32(1);
        data.write_int64(5_000_000);
    });
    let mut reply = transact(
        &mut host,
        connection,
        sensors::connection_codes::SET_EVENT_RATE,
        &mut data,
    )
    .expect("setEventRate answers");
    assert_eq!(
        reply.read_int32().expect("status_t"),
        sensors::BAD_VALUE,
        "a destroyed connection has nothing enabled"
    );
}

// ---------------------------------------------------------------------------
// Hot attach / detach
// ---------------------------------------------------------------------------

/// The proof's last act: detach returns the stock no-sensor answer, on the same
/// binder an app already holds (`core`'s node), and the writer stops.
#[test]
fn detach_returns_the_honest_empty_sensor_list() {
    let (mut host, mut registry) = session();
    let server = host
        .check_service("sensorservice")
        .expect("lookup")
        .expect("binder");
    registry
        .attach(
            &mut host,
            "sensors",
            Profile::Scripted,
            &ModuleConfig::default(),
        )
        .expect("attach");
    let mut data = sensor_list_request("com.acos.sensorprobe");
    let mut reply = transact(
        &mut host,
        server,
        sensors::server_codes::GET_SENSOR_LIST,
        &mut data,
    )
    .expect("list");
    assert_eq!(read_sensor_list(&mut reply).len(), 7);

    registry.detach(&mut host, "sensors").expect("detach");

    // Same binder id: the app's proxy was never invalidated.
    let still = host
        .check_service("sensorservice")
        .expect("lookup")
        .expect("binder");
    assert_eq!(still, server);
    let mut data = sensor_list_request("com.acos.sensorprobe");
    let mut reply = transact(
        &mut host,
        server,
        sensors::server_codes::GET_SENSOR_LIST,
        &mut data,
    )
    .expect("list");
    assert_eq!(
        reply.read_int32().expect("count"),
        0,
        "empty list after detach"
    );
    assert_eq!(reply.data_avail(), 0);

    // A new connection is refused again.
    let mut data = create_connection_request(0);
    let error = transact(
        &mut host,
        server,
        sensors::server_codes::CREATE_SENSOR_EVENT_CONNECTION,
        &mut data,
    )
    .expect_err("no queue after detach");
    assert_eq!(error.code(), "UNSUPPORTED_ANDROID_API");

    // The feed is gone, so a scripted op is refused (`ModuleRegistry::control`
    // refuses a detached module) and the report says `off` again.
    let error = registry
        .control(
            &mut host,
            "sensors",
            "set_value",
            &serde_json::json!({"sensor": "accelerometer", "values": [0.0]}),
        )
        .expect_err("a detached module has no feed");
    assert!(
        matches!(error, ServiceError::ModuleRejected { .. }),
        "{error}"
    );
    let report = registry
        .reports()
        .into_iter()
        .find(|report| report.id == "sensors")
        .expect("report");
    assert!(!report.attached);
    // The writer thread's join is measured by the conformance kit
    // (`detach-restores` counts /proc/self/task in a quiet child process);
    // this test runs next to its siblings, so a global thread count here would
    // be noise.
}

// ---------------------------------------------------------------------------
// Honesty
// ---------------------------------------------------------------------------

/// There is no `real` profile, and the manifest says so; the report names the
/// scripted feed and never claims a measurement.
#[test]
fn there_is_no_real_profile_and_the_report_is_honest() {
    let (mut host, mut registry) = session();
    let manifest = registry.manifest("sensors").expect("manifest");
    assert_eq!(
        manifest.profiles,
        &[Profile::Stub, Profile::Scripted],
        "no `real`: ACOS has no sensor hardware"
    );
    assert!(manifest.summary.contains("no `real` profile"));
    assert!(manifest.validate().is_ok());

    let error = registry
        .attach(
            &mut host,
            "sensors",
            Profile::Real,
            &ModuleConfig::default(),
        )
        .expect_err("real is refused");
    assert!(
        matches!(
            error,
            ServiceError::ModuleRejected {
                module: "sensors",
                ..
            }
        ),
        "{error}"
    );

    registry
        .attach(
            &mut host,
            "sensors",
            Profile::Scripted,
            &ModuleConfig::default(),
        )
        .expect("attach scripted");
    let report = registry
        .reports()
        .into_iter()
        .find(|report| report.id == "sensors")
        .expect("report");
    assert_eq!(report.profile, Some(Profile::Scripted));
    let reason = report.reason.expect("attached reports say what they are");
    assert!(reason.contains("scripted"), "{reason}");
    assert!(reason.contains("not measured"), "{reason}");
    assert!(reason.contains("7 sensors"), "{reason}");

    // The verdict row says `scripted` in `fidelity`, so a pass that relied on
    // this module cannot be read as a hardware pass.
    let modules = registry.verdict_modules();
    let row = modules
        .iter()
        .find(|row| row["id"] == "sensors")
        .expect("verdict row");
    assert_eq!(row["fidelity"], "scripted");
    assert_eq!(row["state"], "attached");
}

/// The `stub` profile is fixed answers: no control op may change it.
#[test]
fn stub_refuses_every_scripted_control() {
    let (mut host, mut registry) = session();
    registry
        .attach(
            &mut host,
            "sensors",
            Profile::Stub,
            &ModuleConfig::default(),
        )
        .expect("attach stub");
    for op in ["set_value", "play", "shake", "tilt", "enable", "disable"] {
        let error = registry
            .control(
                &mut host,
                "sensors",
                op,
                &serde_json::json!({"sensor": "accelerometer", "values": [1.0]}),
            )
            .expect_err("stub must refuse a scripted op");
        assert!(
            matches!(
                error,
                ServiceError::ModuleRejected {
                    module: "sensors",
                    ..
                }
            ),
            "{op}: {error}"
        );
    }
    // A stub still serves a list (that is its fixed answer) and streams its
    // fixed values to a listener.
    let server = host
        .check_service("sensorservice")
        .expect("lookup")
        .expect("binder");
    let mut data = sensor_list_request("com.acos.sensorprobe");
    let mut reply = transact(
        &mut host,
        server,
        sensors::server_codes::GET_SENSOR_LIST,
        &mut data,
    )
    .expect("list");
    assert_eq!(read_sensor_list(&mut reply).len(), 7);
}

/// A config can narrow the table and set initial values; an unknown sensor is a
/// refusal, never a silent default.
#[test]
fn the_launch_config_selects_sensors_and_values() {
    let (mut host, mut registry) = session();
    let config = ModuleConfig::from_json(
        "sensors",
        &serde_json::json!({
            "sensors": ["accelerometer", "light"],
            "values": {"accelerometer": [3.0, 0.0, 9.81], "light": [42.0]},
        }),
    )
    .expect("config");
    registry
        .attach(&mut host, "sensors", Profile::Scripted, &config)
        .expect("attach with config");
    let server = host
        .check_service("sensorservice")
        .expect("lookup")
        .expect("binder");
    let mut data = sensor_list_request("com.acos.sensorprobe");
    let mut reply = transact(
        &mut host,
        server,
        sensors::server_codes::GET_SENSOR_LIST,
        &mut data,
    )
    .expect("list");
    let list = read_sensor_list(&mut reply);
    assert_eq!(list.len(), 2, "only the selected sensors are served");
    assert_eq!(list[0].sensor_type, TYPE_ACCELEROMETER);
    assert_eq!(list[1].sensor_type, TYPE_LIGHT);

    let status = registry
        .control(&mut host, "sensors", "status", &serde_json::json!({}))
        .expect("status");
    assert_eq!(status["sensors"][0]["values"][0], 3.0);
    assert_eq!(status["sensors"][1]["values"][0], 42.0);

    registry.detach(&mut host, "sensors").expect("detach");

    let error = registry
        .attach(
            &mut host,
            "sensors",
            Profile::Scripted,
            &ModuleConfig::from_json("sensors", &serde_json::json!({"sensors": ["pressure"]}))
                .expect("config"),
        )
        .expect_err("pressure is not served");
    assert!(
        matches!(
            error,
            ServiceError::ModuleRejected {
                module: "sensors",
                ..
            }
        ),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// The conformance kit
// ---------------------------------------------------------------------------

/// ADR-0029 item 7: the kit against the shipped module. The detach check counts
/// process-wide threads and fds, so it runs in a quiet child process (the same
/// mechanism as `tests/module_kit.rs`).
#[test]
fn the_sensors_module_passes_the_conformance_kit() {
    let exe = std::env::current_exe().expect("test executable");
    let output = std::process::Command::new(exe)
        .args([
            "--exact",
            "quiet_kit_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("ACOS_KIT_CHILD", "sensors")
        .output()
        .expect("spawn the kit child");
    assert!(
        output.status.success(),
        "the conformance kit for `sensors` failed in a quiet process:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("detach-restores: PASS"), "{stdout}");
    assert!(stdout.contains("honest-report: PASS"), "{stdout}");
}

/// The child half: a no-op unless the parent asked for the kit.
#[test]
fn quiet_kit_child() {
    let Ok(module) = std::env::var("ACOS_KIT_CHILD") else {
        return;
    };
    let report = module::conformance::run(&module);
    for check in &report.checks {
        println!(
            "{}: {} {}",
            check.name,
            if check.pass { "PASS" } else { "FAIL" },
            check.detail
        );
    }
    assert!(
        report.pass(),
        "{} failed the kit: {}",
        report.module,
        report.failures()
    );
}
