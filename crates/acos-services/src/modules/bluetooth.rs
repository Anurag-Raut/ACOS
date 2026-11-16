//! `hw.bluetooth` — Bluetooth as a scripted adapter (stub, scripted).
//!
//! No radio, no bond store, no scan. What the module provides is the two binders
//! the framework's own client classes reach, answering with scripted state:
//!
//! | name | descriptor | what answers |
//! |---|---|---|
//! | `bluetooth_manager` | `android.bluetooth.IBluetoothManager` | `registerAdapter` (hands the adapter binder), `getState`, `getAddress`, `getName`, `enable`/`disable`, `isBleScanAlwaysAvailable`, `enableBle`/`disableBle`, `registerStateChangeCallback` |
//! | `bluetooth` | `android.bluetooth.IBluetooth` | `getState`, `getAddress`, `getName`, `getScanMode`, `startDiscovery`/`cancelDiscovery`/`isDiscovering`, `getBondedDevices` (a real `List<BluetoothDevice>` through the client's `ISynchronousResultReceiver`) |
//!
//! Transaction codes come from the image (`framework-bluetooth/classes.dex`).
//! The two client paths that carry a binder to call back (`getBondedDevices`,
//! `isDiscovering`, `startDiscovery`) get an
//! `ISynchronousResultReceiver.send(Result)` frame: presence word, a null
//! `writeValue` exception, then the value (`VAL_BOOLEAN`, or `VAL_LIST` of
//! `VAL_PARCELABLE` `BluetoothDevice`).
//!
//! ## Events
//!
//! * `state` control op → `ACTION_STATE_CHANGED` broadcast (extras
//!   `EXTRA_STATE`/`EXTRA_PREVIOUS_STATE`) and the registered
//!   `IBluetoothManagerCallback.onBluetoothServiceUp/Down`.
//! * `scan` control op → `ACTION_DISCOVERY_STARTED`, one `ACTION_FOUND` per
//!   scripted device (`EXTRA_DEVICE` is a real `BluetoothDevice` parcel,
//!   `EXTRA_NAME`, `EXTRA_RSSI`) and `ACTION_DISCOVERY_FINISHED`.
//!
//! ## Honest gaps (in every report)
//!
//! * No radio: the state is a script, not a controller; there is no BLE, no
//!   pairing and no real discovery.
//! * GATT/`IBluetoothGatt` is not provided, so LE scanning
//!   (`BluetoothLeScanner.startScan`) answers a structured
//!   `UNSUPPORTED_ANDROID_API` rather than an empty result. Classic discovery
//!   (`BluetoothAdapter.startDiscovery`, `ACTION_FOUND`) is scripted instead.
//! * Profiles (`A2DP`, `HEADSET`, ...) are not provided.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::ams::IntentFacts;
use crate::error::{Result, ServiceError};
use crate::exception::write_no_exception;
use crate::host::{CallResult, PendingDelivery, ServiceHost, ServiceObject};
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass, ServiceProvide,
};
use crate::parcelable;
use crate::value::{DynamicValue, Parcelable};

/// The module id (`TOMBSTONES` already names it for `bluetooth`).
pub const MODULE_ID: &str = "hw.bluetooth";
/// `IBluetoothManager` descriptor.
pub const MANAGER_DESCRIPTOR: &str = "android.bluetooth.IBluetoothManager";
/// `IBluetooth` descriptor.
pub const BLUETOOTH_DESCRIPTOR: &str = "android.bluetooth.IBluetooth";
/// `IBluetoothManagerCallback` descriptor.
pub const MANAGER_CALLBACK_DESCRIPTOR: &str = "android.bluetooth.IBluetoothManagerCallback";
/// `ISynchronousResultReceiver` descriptor (the client's result binder).
pub const RESULT_RECEIVER_DESCRIPTOR: &str = "com.android.modules.utils.ISynchronousResultReceiver";
/// `BluetoothDevice` parcelable class.
pub const DEVICE_CLASS: &str = "android.bluetooth.BluetoothDevice";

/// `BluetoothAdapter.STATE_OFF`.
pub const STATE_OFF: i32 = 10;
/// `BluetoothAdapter.STATE_ON`.
pub const STATE_ON: i32 = 12;
/// `BluetoothAdapter.SCAN_MODE_CONNECTABLE_DISCOVERABLE`.
pub const SCAN_MODE_DISCOVERABLE: i32 = 23;
/// `BluetoothAdapter.SCAN_MODE_NONE`.
pub const SCAN_MODE_NONE: i32 = 20;

/// `IBluetoothManager.registerAdapter(IBluetoothManagerCallback)`.
const MANAGER_REGISTER_ADAPTER: u32 = 1;
/// `IBluetoothManager.registerStateChangeCallback(IBluetoothManagerCallback)`.
const MANAGER_REGISTER_STATE_CHANGE_CALLBACK: u32 = 3;
/// `IBluetoothManager.enable()`.
const MANAGER_ENABLE: u32 = 5;
/// `IBluetoothManager.disable()`.
const MANAGER_DISABLE: u32 = 7;
/// `IBluetoothManager.getState()`.
const MANAGER_GET_STATE: u32 = 8;
/// `IBluetoothManager.getAddress(AttributionSource)`.
const MANAGER_GET_ADDRESS: u32 = 12;
/// `IBluetoothManager.getName(AttributionSource)`.
const MANAGER_GET_NAME: u32 = 13;
/// `IBluetoothManager.isBleScanAlwaysAvailable()`.
const MANAGER_IS_BLE_SCAN_ALWAYS_AVAILABLE: u32 = 15;
/// `IBluetoothManager.enableBle()`.
const MANAGER_ENABLE_BLE: u32 = 16;
/// `IBluetoothManager.disableBle()`.
const MANAGER_DISABLE_BLE: u32 = 17;

/// `IBluetooth.getState()`.
const ADAPTER_GET_STATE: u32 = 1;
/// `IBluetooth.getAddress(AttributionSource)`.
const ADAPTER_GET_ADDRESS: u32 = 4;
/// `IBluetooth.getName(AttributionSource)`.
const ADAPTER_GET_NAME: u32 = 9;
/// `IBluetooth.getScanMode()`.
const ADAPTER_GET_SCAN_MODE: u32 = 13;
/// `IBluetooth.startDiscovery(AttributionSource, SynchronousResultReceiver)`.
const ADAPTER_START_DISCOVERY: u32 = 17;
/// `IBluetooth.cancelDiscovery(AttributionSource, SynchronousResultReceiver)`.
const ADAPTER_CANCEL_DISCOVERY: u32 = 18;
/// `IBluetooth.isDiscovering(AttributionSource, SynchronousResultReceiver)`.
const ADAPTER_IS_DISCOVERING: u32 = 19;
/// `IBluetooth.getBondedDevices(AttributionSource, SynchronousResultReceiver)`.
const ADAPTER_GET_BONDED_DEVICES: u32 = 23;

/// `IBluetoothManagerCallback.onBluetoothServiceUp(IBluetooth)`.
const CALLBACK_SERVICE_UP: u32 = 1;
/// `IBluetoothManagerCallback.onBluetoothServiceDown()`.
const CALLBACK_SERVICE_DOWN: u32 = 2;

/// `BluetoothAdapter.ACTION_STATE_CHANGED`.
pub const ACTION_STATE_CHANGED: &str = "android.bluetooth.adapter.action.STATE_CHANGED";
/// `BluetoothAdapter.EXTRA_STATE`.
pub const EXTRA_STATE: &str = "android.bluetooth.adapter.extra.STATE";
/// `BluetoothAdapter.EXTRA_PREVIOUS_STATE`.
pub const EXTRA_PREVIOUS_STATE: &str = "android.bluetooth.adapter.extra.PREVIOUS_STATE";
/// `BluetoothAdapter.ACTION_DISCOVERY_STARTED`.
pub const ACTION_DISCOVERY_STARTED: &str = "android.bluetooth.adapter.action.DISCOVERY_STARTED";
/// `BluetoothAdapter.ACTION_DISCOVERY_FINISHED`.
pub const ACTION_DISCOVERY_FINISHED: &str = "android.bluetooth.adapter.action.DISCOVERY_FINISHED";
/// `BluetoothDevice.ACTION_FOUND`.
pub const ACTION_FOUND: &str = "android.bluetooth.device.action.FOUND";
/// `BluetoothDevice.EXTRA_DEVICE`.
pub const EXTRA_DEVICE: &str = "android.bluetooth.device.extra.DEVICE";
/// `BluetoothDevice.EXTRA_NAME`.
pub const EXTRA_NAME: &str = "android.bluetooth.device.extra.NAME";
/// `BluetoothDevice.EXTRA_RSSI`.
pub const EXTRA_RSSI: &str = "android.bluetooth.device.extra.RSSI";

/// One scripted Bluetooth device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptedDevice {
    /// Upper-case `AA:BB:CC:DD:EE:FF`.
    pub address: String,
    /// Human-readable name.
    pub name: String,
    /// RSSI for scan results.
    pub rssi: i32,
}

/// The session's Bluetooth state.
#[derive(Debug)]
pub struct BluetoothState {
    /// Adapter on/off.
    pub on: bool,
    /// `BluetoothAdapter.getName()`.
    pub name: String,
    /// `BluetoothAdapter.getAddress()`.
    pub address: String,
    /// Bonded devices (`getBondedDevices`).
    pub bonded: Vec<ScriptedDevice>,
    /// Devices a `scan` control op reports.
    pub scan: Vec<ScriptedDevice>,
    /// `isDiscovering`.
    pub discovering: bool,
    /// `startDiscovery` calls.
    pub start_discovery_calls: u32,
    /// `cancelDiscovery` calls.
    pub cancel_discovery_calls: u32,
    /// `getBondedDevices` calls.
    pub bonded_calls: u32,
    /// `registerAdapter`/state-change callbacks (binders).
    pub callbacks: Vec<u64>,
    /// Calls answered with the structured unsupported error.
    pub unsupported: Vec<String>,
}

impl Default for BluetoothState {
    fn default() -> Self {
        Self {
            on: false,
            name: "ACOS Scripted".to_string(),
            address: "02:00:00:00:00:01".to_string(),
            bonded: vec![ScriptedDevice {
                address: "11:22:33:44:55:66".to_string(),
                name: "ACOS Headset".to_string(),
                rssi: -52,
            }],
            scan: vec![ScriptedDevice {
                address: "AA:BB:CC:DD:EE:01".to_string(),
                name: "ACOS Beacon".to_string(),
                rssi: -40,
            }],
            discovering: false,
            start_discovery_calls: 0,
            cancel_discovery_calls: 0,
            bonded_calls: 0,
            callbacks: Vec::new(),
            unsupported: Vec::new(),
        }
    }
}

impl BluetoothState {
    /// The state as JSON (the `state` control op and `report`).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "on": self.on,
            "state": if self.on { STATE_ON } else { STATE_OFF },
            "name": self.name,
            "address": self.address,
            "bonded": self.bonded.iter().map(device_json).collect::<Vec<_>>(),
            "scan": self.scan.iter().map(device_json).collect::<Vec<_>>(),
            "discovering": self.discovering,
            "start_discovery_calls": self.start_discovery_calls,
            "cancel_discovery_calls": self.cancel_discovery_calls,
            "bonded_calls": self.bonded_calls,
            "callback_count": self.callbacks.len(),
            "unsupported": self.unsupported,
        })
    }
}

fn device_json(device: &ScriptedDevice) -> Value {
    json!({"address": device.address, "name": device.name, "rssi": device.rssi})
}

const CONTROL_OPS: [ControlOp; 5] = [
    ControlOp {
        name: "state",
        args: "none — the scripted adapter state, bonded/scan devices and recorded calls",
    },
    ControlOp {
        name: "set",
        args: "{on?, name?, address?} — set the adapter state; a change delivers ACTION_STATE_CHANGED and calls onBluetoothServiceUp/Down",
    },
    ControlOp {
        name: "bonded",
        args: "{devices: [{address, name, rssi?}]} — replace the bonded-device list `getBondedDevices` answers",
    },
    ControlOp {
        name: "scan",
        args: "{devices?: [{address, name, rssi?}]} — run a scripted discovery: ACTION_DISCOVERY_STARTED, one ACTION_FOUND per device, ACTION_DISCOVERY_FINISHED",
    },
    ControlOp {
        name: "reset",
        args: "none — clear the recorded calls (not the scripted state)",
    },
];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "bluetooth: scripted adapter state, name/address, bonded-device list and classic-discovery results (no radio, no BLE/GATT)",
        features: &["android.hardware.bluetooth"],
        services: &[
            ServiceProvide {
                name: "bluetooth_manager",
                descriptor: MANAGER_DESCRIPTOR,
            },
            ServiceProvide {
                name: "bluetooth",
                descriptor: BLUETOOTH_DESCRIPTOR,
            },
        ],
        providers: &[],
        broadcasts: &[
            ACTION_STATE_CHANGED,
            ACTION_DISCOVERY_STARTED,
            ACTION_DISCOVERY_FINISHED,
            ACTION_FOUND,
        ],
        windows: &[],
        system_apps: &[],
        requires: &[],
        requires_host_tools: &[],
        profiles: &[Profile::Stub, Profile::Scripted],
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

/// The module.
pub struct BluetoothModule {
    profile: Profile,
    state: Option<Arc<Mutex<BluetoothState>>>,
}

impl BluetoothModule {
    /// A detached module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: Profile::Off,
            state: None,
        }
    }

    /// The manifest (used by the tests and the registry).
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        manifest_ref()
    }
}

impl Default for BluetoothModule {
    fn default() -> Self {
        Self::new()
    }
}

fn rejected(detail: impl Into<String>) -> ServiceError {
    ServiceError::ModuleRejected {
        module: MODULE_ID,
        detail: detail.into(),
    }
}

fn state_of(state: &Arc<Mutex<BluetoothState>>) -> std::sync::MutexGuard<'_, BluetoothState> {
    state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `Parcel.writeString(address)` + `Parcel.writeInt(addressType)` in a
/// `VAL_PARCELABLE` (`BluetoothDevice.writeToParcel`).
fn device_parcelable(device: &ScriptedDevice) -> Parcelable {
    Parcelable::ordered(
        DEVICE_CLASS,
        vec![
            crate::value::Value::Str(Some(device.address.clone())),
            crate::value::Value::Int(0), // ADDRESS_TYPE_PUBLIC
        ],
    )
}

/// Find the client's `ISynchronousResultReceiver` in a oneway request.
///
/// `IBluetooth.startDiscovery`/`isDiscovering`/`getBondedDevices` write
/// `(AttributionSource, SynchronousResultReceiver)`, and the receiver's
/// `writeToParcel` is one `writeStrongBinder`. The `AttributionSource` carries
/// data only, so the **last** binder object in the request is the receiver: it is
/// found from the parcel's object table, not by guessing an
/// `AttributionSourceState` layout ACOS does not model.
fn result_receiver(data: &acos_parcel::Parcel) -> Option<u64> {
    let mut probe = data.clone();
    for &offset in data.objects().iter().rev() {
        probe.set_data_position(offset);
        if let Ok(Some(id)) = probe.read_strong_binder() {
            return Some(id.0);
        }
    }
    None
}

/// The `ISynchronousResultReceiver.send(Result)` frame.
///
/// `Result.writeToParcel` is `writeValue(mException)` then `writeValue(mObject)`,
/// and AIDL writes the `Result` parameter as a typed object (presence word, no
/// class name).
fn result_delivery(target: u64, value: DynamicValue) -> Option<PendingDelivery> {
    let mut data = acos_parcel::Parcel::new();
    data.write_interface_token(RESULT_RECEIVER_DESCRIPTOR).ok()?;
    data.write_int32(1); // `Result` is a present typed object
    parcelable::write_dynamic(&mut data, &DynamicValue::Null).ok()?;
    parcelable::write_dynamic(&mut data, &value).ok()?;
    Some(PendingDelivery {
        target: acos_parcel::BinderId(target),
        interface: RESULT_RECEIVER_DESCRIPTOR.to_string(),
        code: 1,
        method: "send".to_string(),
        data,
    })
}

struct ManagerService {
    state: Arc<Mutex<BluetoothState>>,
    /// The adapter binder `registerAdapter` hands out.
    adapter_binder: u64,
}

impl ServiceObject for ManagerService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: format!("{MODULE_ID}: {} has no stub answer", method.name),
        })
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        if interface != MANAGER_DESCRIPTOR {
            return Ok(false);
        }
        let mut state = state_of(&self.state);
        match code {
            MANAGER_REGISTER_ADAPTER => {
                // `IBluetooth registerAdapter(IBluetoothManagerCallback)`: the
                // return value is the adapter binder. A `BluetoothManager` whose
                // adapter is null (AOSP's no-Bluetooth path) never gets here.
                if let Ok(Some(callback)) = data.read_strong_binder() {
                    state.callbacks.push(callback.0);
                }
                write_no_exception(reply);
                reply.write_strong_binder(acos_parcel::BinderId(self.adapter_binder));
                Ok(true)
            }
            MANAGER_REGISTER_STATE_CHANGE_CALLBACK => {
                if let Ok(Some(callback)) = data.read_strong_binder() {
                    state.callbacks.push(callback.0);
                }
                write_no_exception(reply);
                Ok(true)
            }
            MANAGER_ENABLE | MANAGER_DISABLE => {
                let on = code == MANAGER_ENABLE;
                state.on = on;
                write_no_exception(reply);
                reply.write_int32(1);
                Ok(true)
            }
            MANAGER_GET_STATE => {
                write_no_exception(reply);
                reply.write_int32(if state.on { STATE_ON } else { STATE_OFF });
                Ok(true)
            }
            MANAGER_GET_ADDRESS => {
                let address = state.address.clone();
                write_no_exception(reply);
                reply
                    .write_string16_opt(Some(&address))
                    .map_err(ServiceError::Parcel)?;
                Ok(true)
            }
            MANAGER_GET_NAME => {
                let name = state.name.clone();
                write_no_exception(reply);
                reply
                    .write_string16_opt(Some(&name))
                    .map_err(ServiceError::Parcel)?;
                Ok(true)
            }
            MANAGER_IS_BLE_SCAN_ALWAYS_AVAILABLE => {
                // No BLE stack: always-available scanning is false, which is the
                // honest answer on a device without the feature.
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            MANAGER_ENABLE_BLE | MANAGER_DISABLE_BLE => {
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

struct AdapterService {
    state: Arc<Mutex<BluetoothState>>,
    queued: Vec<PendingDelivery>,
}

impl ServiceObject for AdapterService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: format!("{MODULE_ID}: {} has no stub answer", method.name),
        })
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        if interface != BLUETOOTH_DESCRIPTOR {
            return Ok(false);
        }
        let mut state = state_of(&self.state);
        match code {
            ADAPTER_GET_STATE => {
                write_no_exception(reply);
                reply.write_int32(if state.on { STATE_ON } else { STATE_OFF });
                Ok(true)
            }
            ADAPTER_GET_ADDRESS => {
                let address = state.address.clone();
                write_no_exception(reply);
                reply
                    .write_string16_opt(Some(&address))
                    .map_err(ServiceError::Parcel)?;
                Ok(true)
            }
            ADAPTER_GET_NAME => {
                let name = state.name.clone();
                write_no_exception(reply);
                reply
                    .write_string16_opt(Some(&name))
                    .map_err(ServiceError::Parcel)?;
                Ok(true)
            }
            ADAPTER_GET_SCAN_MODE => {
                let mode = if state.on {
                    SCAN_MODE_DISCOVERABLE
                } else {
                    SCAN_MODE_NONE
                };
                write_no_exception(reply);
                reply.write_int32(mode);
                Ok(true)
            }
            ADAPTER_START_DISCOVERY | ADAPTER_CANCEL_DISCOVERY | ADAPTER_IS_DISCOVERING
            | ADAPTER_GET_BONDED_DEVICES => {
                // All four are `oneway` and answer through the client's
                // `ISynchronousResultReceiver`; a oneway reply has no header.
                let Some(target) = result_receiver(data) else {
                    return Err(ServiceError::BadRequest {
                        interface: BLUETOOTH_DESCRIPTOR.to_string(),
                        method: format!("code {code}"),
                        detail: "no ISynchronousResultReceiver binder in the request".into(),
                    });
                };
                match code {
                    ADAPTER_START_DISCOVERY => {
                        if !state.on {
                            return Err(rejected(
                                "startDiscovery: the scripted adapter is off; AOSP answers a discovery start on an off adapter with false",
                            ));
                        }
                        state.start_discovery_calls += 1;
                        state.discovering = true;
                        if let Some(delivery) = result_delivery(target, DynamicValue::Bool(true)) {
                            self.queued.push(delivery);
                        }
                    }
                    ADAPTER_CANCEL_DISCOVERY => {
                        state.cancel_discovery_calls += 1;
                        state.discovering = false;
                        if let Some(delivery) = result_delivery(target, DynamicValue::Bool(true)) {
                            self.queued.push(delivery);
                        }
                    }
                    ADAPTER_IS_DISCOVERING => {
                        let discovering = state.discovering;
                        if let Some(delivery) =
                            result_delivery(target, DynamicValue::Bool(discovering))
                        {
                            self.queued.push(delivery);
                        }
                    }
                    _ => {
                        state.bonded_calls += 1;
                        let devices: Vec<DynamicValue> = if state.on {
                            state
                                .bonded
                                .iter()
                                .map(|device| DynamicValue::Parcelable(device_parcelable(device)))
                                .collect()
                        } else {
                            // AOSP's client returns an empty set when the adapter
                            // is not on before it ever transacts; answering an
                            // empty list keeps the same meaning if it does ask.
                            Vec::new()
                        };
                        if let Some(delivery) =
                            result_delivery(target, DynamicValue::List(Some(devices)))
                        {
                            self.queued.push(delivery);
                        }
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn drain_raw_deliveries(&mut self) -> Vec<PendingDelivery> {
        std::mem::take(&mut self.queued)
    }
}

fn parse_address(text: &str) -> Result<String> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() != 6
        || !parts
            .iter()
            .all(|part| part.len() == 2 && part.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return Err(rejected(format!(
            "bluetooth address {text:?} is not AA:BB:CC:DD:EE:FF"
        )));
    }
    Ok(text.to_ascii_uppercase())
}

fn parse_devices(value: &Value) -> Result<Vec<ScriptedDevice>> {
    let Some(items) = value.as_array() else {
        return Err(rejected("devices must be an array of {address, name, rssi?}"));
    };
    items
        .iter()
        .map(|item| {
            let Some(address) = item.get("address").and_then(Value::as_str) else {
                return Err(rejected("a device needs a string `address`"));
            };
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                return Err(rejected("a device needs a string `name`"));
            };
            let rssi = item
                .get("rssi")
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or(-55);
            Ok(ScriptedDevice {
                address: parse_address(address)?,
                name: name.to_string(),
                rssi,
            })
        })
        .collect()
}

fn apply_set(state: &mut BluetoothState, args: &Value) -> Result<()> {
    let Some(map) = args.as_object() else {
        return Err(rejected("`set` needs an object of scripted fields"));
    };
    for (key, value) in map {
        match key.as_str() {
            "on" => match value {
                Value::Bool(flag) => state.on = *flag,
                other => return Err(rejected(format!("on must be a boolean, got {other}"))),
            },
            "name" => {
                let Some(text) = value.as_str() else {
                    return Err(rejected("name must be a string"));
                };
                state.name = text.to_string();
            }
            "address" => {
                let Some(text) = value.as_str() else {
                    return Err(rejected("address must be a string"));
                };
                state.address = parse_address(text)?;
            }
            other => {
                return Err(rejected(format!(
                    "unknown scripted field {other:?}; this module accepts on, name, address"
                )))
            }
        }
    }
    Ok(())
}

fn state_changed(previous: bool, now: bool) -> Option<(i32, i32)> {
    (previous != now).then_some((
        if now { STATE_ON } else { STATE_OFF },
        if previous { STATE_ON } else { STATE_OFF },
    ))
}

impl Module for BluetoothModule {
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
            return Err(rejected(format!(
                "{profile} is not implemented: ACOS has no Bluetooth controller, so use `stub` or `scripted`"
            )));
        }
        let mut state = BluetoothState::default();
        let mut direct = serde_json::Map::new();
        for (key, value) in config.to_json().as_object().cloned().unwrap_or_default() {
            match key.as_str() {
                "bonded" => state.bonded = parse_devices(&value)?,
                "scan" => state.scan = parse_devices(&value)?,
                _ => {
                    direct.insert(key, value);
                }
            }
        }
        if !direct.is_empty() {
            apply_set(&mut state, &Value::Object(direct))?;
        }
        let state = Arc::new(Mutex::new(state));
        let adapter = host.register_dynamic_named(
            "bluetooth",
            BLUETOOTH_DESCRIPTOR,
            Box::new(AdapterService {
                state: Arc::clone(&state),
                queued: Vec::new(),
            }),
        );
        host.register_dynamic_named(
            "bluetooth_manager",
            MANAGER_DESCRIPTOR,
            Box::new(ManagerService {
                state: Arc::clone(&state),
                adapter_binder: adapter.0,
            }),
        );
        self.state = Some(state);
        self.profile = profile;
        Ok(())
    }

    fn detach(&mut self, _host: &mut ServiceHost) -> Result<()> {
        self.state = None;
        self.profile = Profile::Off;
        Ok(())
    }

    fn control(&mut self, host: &mut ServiceHost, op: &str, args: &Value) -> Result<Value> {
        let Some(shared) = self.state.as_ref().cloned() else {
            return Err(rejected(format!("module is off, so {op:?} was not applied")));
        };
        match op {
            "state" => Ok(state_of(&shared).to_json()),
            "set" => {
                let (previous, now, callbacks) = {
                    let mut state = state_of(&shared);
                    let previous = state.on;
                    apply_set(&mut state, args)?;
                    (previous, state.on, state.callbacks.clone())
                };
                if let Some((now_state, previous_state)) = state_changed(previous, now) {
                    let mut intent = IntentFacts::action(ACTION_STATE_CHANGED);
                    intent.extras.insert(
                        EXTRA_STATE.to_string(),
                        crate::ams::Extra::Int(now_state),
                    );
                    intent.extras.insert(
                        EXTRA_PREVIOUS_STATE.to_string(),
                        crate::ams::Extra::Int(previous_state),
                    );
                    let matched = host.broadcast_intent(&intent)?;
                    // `IBluetoothManagerCallback` gets the service up/down call
                    // the framework's own manager would send.
                    let mut delivered = 0;
                    for callback in callbacks {
                        let mut data = acos_parcel::Parcel::new();
                        data.write_interface_token(MANAGER_CALLBACK_DESCRIPTOR)
                            .ok();
                        let code = if now {
                            CALLBACK_SERVICE_UP
                        } else {
                            CALLBACK_SERVICE_DOWN
                        };
                        if now {
                            data.write_strong_binder(acos_parcel::BinderId(
                                host.binder_for_descriptor(BLUETOOTH_DESCRIPTOR)
                                    .map(|binder| binder.0)
                                    .unwrap_or(0),
                            ));
                        }
                        host.queue_delivery(PendingDelivery {
                            target: acos_parcel::BinderId(callback),
                            interface: MANAGER_CALLBACK_DESCRIPTOR.to_string(),
                            code,
                            method: "stateChangeCallback".to_string(),
                            data,
                        });
                        delivered += 1;
                    }
                    Ok(json!({
                        "on": now,
                        "state": if now { STATE_ON } else { STATE_OFF },
                        "broadcast_matched": matched,
                        "callbacks_delivered": delivered,
                    }))
                } else {
                    Ok(json!({
                        "on": now,
                        "state": if now { STATE_ON } else { STATE_OFF },
                        "broadcast_matched": 0,
                        "callbacks_delivered": 0,
                    }))
                }
            }
            "bonded" => {
                let devices = parse_devices(
                    args.get("devices")
                        .ok_or_else(|| rejected("bonded needs {devices: [...]}"))?,
                )?;
                let mut state = state_of(&shared);
                state.bonded = devices;
                Ok(state.to_json())
            }
            "scan" => {
                let devices = match args.get("devices") {
                    Some(value) => {
                        let mut state = state_of(&shared);
                        state.scan = parse_devices(value)?;
                        state.scan.clone()
                    }
                    None => state_of(&shared).scan.clone(),
                };
                {
                    let mut state = state_of(&shared);
                    if !state.on {
                        return Err(rejected(
                            "scan: the scripted adapter is off; attach a device state with `set {on: true}` first (AOSP broadcasts no discovery results on an off adapter)",
                        ));
                    }
                    state.discovering = true;
                }
                let mut matched = 0;
                matched += host.broadcast_intent(&IntentFacts::action(ACTION_DISCOVERY_STARTED))?;
                for device in &devices {
                    let mut intent = IntentFacts::action(ACTION_FOUND);
                    intent.extras.insert(
                        EXTRA_DEVICE.to_string(),
                        crate::ams::Extra::Parcelable(device_parcelable(device)),
                    );
                    intent.extras.insert(
                        EXTRA_NAME.to_string(),
                        crate::ams::Extra::String(device.name.clone()),
                    );
                    intent
                        .extras
                        .insert(EXTRA_RSSI.to_string(), crate::ams::Extra::Int(device.rssi));
                    matched += host.broadcast_intent(&intent)?;
                }
                matched += host.broadcast_intent(&IntentFacts::action(ACTION_DISCOVERY_FINISHED))?;
                state_of(&shared).discovering = false;
                Ok(json!({
                    "devices": devices.iter().map(device_json).collect::<Vec<_>>(),
                    "broadcast_matched": matched,
                }))
            }
            "reset" => {
                let mut state = state_of(&shared);
                state.start_discovery_calls = 0;
                state.cancel_discovery_calls = 0;
                state.bonded_calls = 0;
                state.unsupported.clear();
                Ok(json!({"reset": true}))
            }
            other => Err(rejected(format!(
                "unknown control op {other:?}; this module implements {}",
                CONTROL_OPS
                    .iter()
                    .map(|op| op.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    fn report(&self) -> ModuleReport {
        let mut report = ModuleReport::of(manifest_ref(), self.profile);
        if self.profile.is_attached() {
            let state = self
                .state
                .as_ref()
                .map(|state| state_of(state).to_json())
                .unwrap_or(Value::Null);
            let bonded = state
                .get("bonded")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            report.reason = Some(format!(
                "{} profile: scripted adapter state ({}), {bonded} bonded device(s), classic discovery via ACTION_FOUND; no radio, no pairing, no BLE/GATT (`BluetoothLeScanner` answers UNSUPPORTED_ANDROID_API), no profiles",
                self.profile,
                if state.get("on").and_then(Value::as_bool).unwrap_or(false) {
                    "on"
                } else {
                    "off"
                }
            ));
        }
        report
    }
}
