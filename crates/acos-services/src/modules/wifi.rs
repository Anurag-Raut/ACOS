//! `net.wifi` — a Wi-Fi client radio the agent can script (`stub`, `scripted`).
//!
//! ACOS has no radio, so there is no `real` profile: a module that claimed one
//! while doing nothing would be the fake success ADR-0029 forbids. What the
//! module *does* model is the part an app can observe through `WifiManager`:
//! whether Wi-Fi is on, which access point it is associated with (SSID, BSSID,
//! RSSI, frequency, link speed), the last scan results, and the broadcasts and
//! callbacks that announce a change. **Off** is `core`'s answer: no `wifi`
//! service name, which is Android's own no-Wi-Fi path (`WifiFrameworkInitializer`
//! returns a null manager when `FEATURE_WIFI` is absent; see the tombstone row).
//!
//! # What `stub` and `scripted` answer
//!
//! `IWifiManager` (image dex, `android.net.wifi.IWifiManager`), served through
//! `raw_transact` because `WifiInfo`/`ScanResult` carry parcelables the generic
//! codec has no model for (their writers are `crate::parcelable::SPECS` rows,
//! pinned byte-for-byte against the image's `framework-wifi` dex by
//! `tests/wifi_module.rs`):
//!
//! | code | method | answer |
//! |---|---|---|
//! | 1 | `getSupportedFeatures()` | `WIFI_FEATURE_INFRA` only (2.4 GHz station) |
//! | 35 | `startScan()` | `true` when Wi-Fi is on; ends in `SCAN_RESULTS_AVAILABLE` (broadcast + every `IScanResultsCallback`) |
//! | 36 | `getScanResults()` | the scripted list; `SecurityException` without scan access |
//! | 38/39/40 | `disconnect()`/`reconnect()`/`reassociate()` | `false`: a third-party app may not change the connection (AOSP returns `false` for non-privileged callers) |
//! | 41 | `getConnectionInfo()` | the associated `WifiInfo`, or the disconnected one (`<unknown ssid>`, RSSI −127) |
//! | 42 | `setWifiEnabled(boolean)` | `false`, state unchanged: an app targeting API 29+ may not toggle Wi-Fi (AOSP `WifiServiceImpl.setWifiEnabled`) |
//! | 43 | `getWifiEnabledState()` | `WIFI_STATE_ENABLED` / `WIFI_STATE_DISABLED` |
//! | 48 | `getCountryCode()` | the configured country (`US`) |
//! | 52–56 | `is24GHz/5GHz/6GHz/60GHzBandSupported`, `isWifiStandardSupported` | 2.4 GHz and 802.11 legacy/n only |
//! | 57 | `getDhcpInfo()` | the session's address block when the `network` module is attached, **zeros** otherwise |
//! | 59 | `isScanAlwaysAvailable()` | `false` |
//! | 60–62, 63–66 | `acquireWifiLock`…, multicast locks | accepted and counted |
//! | 98 | `getVerboseLoggingLevel()` | `0` |
//! | 101 | `getCurrentNetwork()` | the default network while connected and up, `null` otherwise |
//! | 131/132 | `register/unregisterScanResultsCallback` | stored (host-held reference) |
//! | 137 | `calculateSignalLevel(int)` | the platform thresholds (−88, −77, −66, −55) |
//!
//! Everything else answers a structured `UNSUPPORTED_ANDROID_API` naming the
//! method — never a canned success (`getConfiguredNetworks`, `connect`, hotspot,
//! P2P, suggestions…).
//!
//! # Connectivity (no fake connectivity)
//!
//! Wi-Fi moves the session's **default network** (the `connectivity` service's
//! one emulated WIFI-transport network) and nothing else. `disconnect` → the
//! network is lost: every registered `NetworkCallback` gets `onLost`,
//! `getActiveNetwork()` answers `null`; `connect` → the network is available
//! again (`onAvailable` + capabilities + link properties + blocked status),
//! unless the `network` module's own `offline` control holds it down. The
//! capabilities stay what the `network` module says they are: **never
//! `VALIDATED`, no addresses, no DHCP data without it** — a connected Wi-Fi with
//! the `network` module off is "connected, no internet", the same honest pair
//! `ConnectivityManager` has always reported.
//!
//! With the `network` module `real`, the Wi-Fi link *is* the relay veth, so a
//! script that drops it (`disconnect`, `enable {on: false}`) while traffic would
//! keep flowing is refused with a structured error that names `acos_net offline`
//! (the one control that drops the packets as well). `rssi`, `scan` and `connect`
//! (a no-op state change while already connected) stay available.
//!
//! # Control ops (`scripted` only; `stub` is fixed)
//!
//! * `state` / `reset` (any profile);
//! * `enable {on, autoconnect?}` — `WIFI_STATE_CHANGED` (`ENABLING→ENABLED` /
//!   `DISABLING→DISABLED`); turning it off drops the connection;
//! * `connect {ssid?, bssid?, rssi?, frequency?, link_speed?}` —
//!   `NETWORK_STATE_CHANGED` (CONNECTED, with `networkInfo`/`bssid`/`wifiInfo`)
//!   and the `onAvailable` callbacks;
//! * `disconnect` — `NETWORK_STATE_CHANGED` (DISCONNECTED) and `onLost`;
//! * `rssi {rssi}` — `RSSI_CHANGED`;
//! * `scan {results?}` — replaces the scan list and completes a scan.

use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Value};

use crate::ams::{Extra, IntentFacts};
use crate::error::{Result, ServiceError};
use crate::exception::{write_no_exception, EX_SECURITY};
use crate::host::{CallResult, PendingDelivery, ServiceHost, ServiceObject};
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass, ServiceProvide,
};
use crate::parcelable;
use crate::refs::GuestRefs;
use crate::value::{Parcelable, Value as PV};

/// The module id.
pub const MODULE_ID: &str = "net.wifi";
/// The `ServiceManager` name this module owns.
pub const SERVICE_NAME: &str = "wifi";
/// The AIDL descriptor of the interface it serves.
pub const DESCRIPTOR: &str = "android.net.wifi.IWifiManager";
/// `IScanResultsCallback` (the app's `registerScanResultsCallback` binder).
pub const SCAN_CALLBACK_DESCRIPTOR: &str = "android.net.wifi.IScanResultsCallback";

/// `WifiManager.WIFI_STATE_CHANGED_ACTION`.
pub const ACTION_WIFI_STATE_CHANGED: &str = "android.net.wifi.WIFI_STATE_CHANGED";
/// `WifiManager.NETWORK_STATE_CHANGED_ACTION`.
pub const ACTION_NETWORK_STATE_CHANGED: &str = "android.net.wifi.STATE_CHANGE";
/// `WifiManager.RSSI_CHANGED_ACTION`.
pub const ACTION_RSSI_CHANGED: &str = "android.net.wifi.RSSI_CHANGED";
/// `WifiManager.SCAN_RESULTS_AVAILABLE_ACTION`.
pub const ACTION_SCAN_RESULTS: &str = "android.net.wifi.SCAN_RESULTS";

/// `WifiManager.WIFI_STATE_*`.
pub const WIFI_STATE_DISABLING: i32 = 0;
/// `WifiManager.WIFI_STATE_DISABLED`.
pub const WIFI_STATE_DISABLED: i32 = 1;
/// `WifiManager.WIFI_STATE_ENABLING`.
pub const WIFI_STATE_ENABLING: i32 = 2;
/// `WifiManager.WIFI_STATE_ENABLED`.
pub const WIFI_STATE_ENABLED: i32 = 3;

/// `SupplicantState` names (`writeToParcel` writes `name()`, image dex).
const SUPPLICANT_DISCONNECTED: &str = "DISCONNECTED";
const SUPPLICANT_INTERFACE_DISABLED: &str = "INTERFACE_DISABLED";
const SUPPLICANT_COMPLETED: &str = "COMPLETED";

/// `WifiInfo.INVALID_RSSI`.
pub const INVALID_RSSI: i32 = -127;
/// `WifiInfo.DEFAULT_MAC_ADDRESS` (what a non-privileged app is told).
pub const DEFAULT_MAC: &str = "02:00:00:00:00:00";
/// The platform's default RSSI level thresholds (`config_wifiRssiLevelThresholds`).
const RSSI_THRESHOLDS: [i32; 4] = [-88, -77, -66, -55];

const ASSOCIATED_NETWORK_ID: i32 = 0;

const CONFIG_KEYS: [&str; 10] = [
    "ssid",
    "bssid",
    "rssi",
    "frequency",
    "link_speed",
    "capabilities",
    "networks",
    "scan_access",
    "country",
    "enabled",
];

/// One access point in the scan list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessPoint {
    /// SSID (UTF-8).
    pub ssid: String,
    /// BSSID (`aa:bb:cc:dd:ee:ff`).
    pub bssid: String,
    /// Signal strength in dBm.
    pub rssi: i32,
    /// Centre frequency in MHz.
    pub frequency: i32,
    /// `ScanResult.capabilities` (`[WPA2-PSK-CCMP][ESS]`).
    pub capabilities: String,
}

impl AccessPoint {
    fn to_json(&self) -> Value {
        json!({
            "ssid": self.ssid,
            "bssid": self.bssid,
            "rssi": self.rssi,
            "frequency": self.frequency,
            "capabilities": self.capabilities,
        })
    }
}

/// The scripted radio.
#[derive(Debug)]
pub struct WifiState {
    /// Wi-Fi switched on.
    pub enabled: bool,
    /// Associated with [`Self::ssid`].
    pub connected: bool,
    /// The access point the session is (or would be) associated with.
    pub ssid: String,
    /// Its BSSID.
    pub bssid: String,
    /// Current RSSI in dBm.
    pub rssi: i32,
    /// Frequency in MHz.
    pub frequency: i32,
    /// Link speed in Mbps.
    pub link_speed: i32,
    /// `capabilities` of the associated network.
    pub capabilities: String,
    /// The last scan results.
    pub scan: Vec<AccessPoint>,
    /// Whether callers may read scan results / the SSID (location access).
    pub scan_access: bool,
    /// Regulatory country code.
    pub country: String,
    /// The default network is up (connected, and the `network` module has not
    /// taken it down). What `getCurrentNetwork` answers from.
    pub network_up: bool,
    /// `IScanResultsCallback` binders (ids).
    pub callbacks: Vec<u64>,
    /// Those callbacks as host-held strong references.
    pub refs: GuestRefs,
    /// `getConnectionInfo()` calls.
    pub info_calls: u32,
    /// `getScanResults()` calls.
    pub scan_result_calls: u32,
    /// `startScan()` calls.
    pub start_scan_calls: u32,
    /// `setWifiEnabled()` calls (all answered `false`).
    pub set_enabled_calls: u32,
    /// Wi-Fi/multicast lock acquisitions.
    pub lock_calls: u32,
    /// Calls answered with a structured unsupported error.
    pub unsupported: Vec<String>,
    started: std::time::Instant,
}

impl WifiState {
    fn new() -> Self {
        let ssid = "AndroidWifi".to_string();
        let bssid = "00:13:10:85:fe:01".to_string();
        Self {
            enabled: true,
            connected: true,
            scan: vec![AccessPoint {
                ssid: ssid.clone(),
                bssid: bssid.clone(),
                rssi: -55,
                frequency: 2437,
                capabilities: "[WPA2-PSK-CCMP][ESS]".to_string(),
            }],
            ssid,
            bssid,
            rssi: -55,
            frequency: 2437,
            link_speed: 72,
            capabilities: "[WPA2-PSK-CCMP][ESS]".to_string(),
            scan_access: true,
            country: "US".to_string(),
            network_up: !crate::network::offline(),
            callbacks: Vec::new(),
            refs: GuestRefs::new(),
            info_calls: 0,
            scan_result_calls: 0,
            start_scan_calls: 0,
            set_enabled_calls: 0,
            lock_calls: 0,
            unsupported: Vec::new(),
            started: std::time::Instant::now(),
        }
    }

    /// `WifiManager.getWifiState()`.
    #[must_use]
    pub fn wifi_state(&self) -> i32 {
        if self.enabled {
            WIFI_STATE_ENABLED
        } else {
            WIFI_STATE_DISABLED
        }
    }

    /// Associated and the radio is on.
    #[must_use]
    pub fn is_associated(&self) -> bool {
        self.enabled && self.connected
    }

    fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "wifi_state": self.wifi_state(),
            "connected": self.is_associated(),
            "ssid": self.ssid,
            "bssid": self.bssid,
            "rssi": self.rssi,
            "frequency": self.frequency,
            "link_speed": self.link_speed,
            "capabilities": self.capabilities,
            "country": self.country,
            "scan_access": self.scan_access,
            "default_network_up": self.network_up,
            "scan_results": self.scan.iter().map(AccessPoint::to_json).collect::<Vec<_>>(),
            "callbacks": self.callbacks.len(),
            "counters": {
                "get_connection_info": self.info_calls,
                "get_scan_results": self.scan_result_calls,
                "start_scan": self.start_scan_calls,
                "set_wifi_enabled": self.set_enabled_calls,
                "locks": self.lock_calls,
            },
            "unsupported": self.unsupported,
        })
    }
}

fn lock(state: &Arc<Mutex<WifiState>>) -> MutexGuard<'_, WifiState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn rejected(detail: impl Into<String>) -> ServiceError {
    ServiceError::ModuleRejected {
        module: MODULE_ID,
        detail: detail.into(),
    }
}

// ---------------------------------------------------------------------------
// Parcelable builders (writers are `parcelable::SPECS` rows)
// ---------------------------------------------------------------------------

fn ssid_parcelable(ssid: &str) -> Parcelable {
    Parcelable::ordered(
        "android.net.wifi.WifiSsid",
        vec![PV::ByteArray(Some(ssid.as_bytes().to_vec()))],
    )
}

/// `android.net.wifi.WifiInfo` for the current state, as `redacted`-or-not.
///
/// The disconnected form is what AOSP's `WifiInfo.reset()` produces (`SSID`
/// absent → `<unknown ssid>`, RSSI −127, speeds/frequency −1, network id −1);
/// the associated form carries the scripted facts. When the caller has no scan
/// access the SSID/BSSID are redacted exactly as `WifiInfo.makeCopy` does
/// (`shouldRedactLocationSensitiveFields`).
#[must_use]
pub fn wifi_info_parcelable(state: &WifiState) -> Parcelable {
    let associated = state.is_associated();
    let visible = associated && state.scan_access;
    let ip = if associated && crate::network::session_has_network() {
        PV::Cond {
            present: true,
            values: vec![PV::ByteArray(Some(
                crate::connectivity::NET_GUEST_ADDRESS.to_vec(),
            ))],
        }
    } else {
        PV::Cond {
            present: false,
            values: Vec::new(),
        }
    };
    let ssid = if visible {
        PV::Cond {
            present: true,
            values: vec![PV::Parcelable(ssid_parcelable(&state.ssid))],
        }
    } else {
        PV::Cond {
            present: false,
            values: Vec::new(),
        }
    };
    let supplicant = if associated {
        SUPPLICANT_COMPLETED
    } else if state.enabled {
        SUPPLICANT_DISCONNECTED
    } else {
        SUPPLICANT_INTERFACE_DISABLED
    };
    let (rssi, link, frequency, standard, max_link) = if associated {
        (state.rssi, state.link_speed, state.frequency, 4, state.link_speed)
    } else {
        (INVALID_RSSI, -1, -1, 0, -1)
    };
    let bssid = if associated {
        if state.scan_access {
            Some(state.bssid.clone())
        } else {
            Some(DEFAULT_MAC.to_string())
        }
    } else {
        None
    };
    Parcelable::ordered(
        "android.net.wifi.WifiInfo",
        vec![
            PV::Int(if visible { ASSOCIATED_NETWORK_ID } else { -1 }), // mNetworkId
            PV::Int(rssi),
            PV::Int(link),
            PV::Int(link), // tx link speed
            PV::Int(link), // rx link speed
            PV::Int(frequency),
            ip,
            ssid,
            PV::Str(bssid),
            PV::Str(Some(DEFAULT_MAC.to_string())),
            PV::Int(0), // metered hint
            PV::Int(0), // ephemeral
            PV::Int(0), // trusted
            PV::Int(0), // oem paid
            PV::Int(0), // oem private
            PV::Int(0), // carrier merged
            PV::Int(0), // score
            PV::Bool(associated),
            PV::Long(0),
            PV::Double(0.0),
            PV::Long(0),
            PV::Double(0.0),
            PV::Long(0),
            PV::Double(0.0),
            PV::Long(0),
            PV::Double(0.0),
            PV::Parcelable(Parcelable::ordered(
                "android.net.wifi.SupplicantState",
                vec![PV::Str(Some(supplicant.to_string()))],
            )),
            PV::Int(0), // osu ap
            PV::Str(None),
            PV::Str(None),
            PV::Str(None),
            PV::Int(standard),
            PV::Int(max_link),
            PV::Int(max_link),
            PV::Str(None),
            PV::Int(-1),     // subscription id
            PV::List(None),  // information elements
            PV::Int(i32::from(associated)), // mIsPrimary
            PV::Int(if associated { 2 } else { -1 }), // PSK / unknown
            PV::Int(0),      // restricted
            PV::Str(None),   // network key
            PV::NamedParcelable(Parcelable::Null),
            PV::Int(-1),     // ap mlo link id
            PV::List(Some(Vec::new())),
            PV::Bool(false),
        ],
    )
}

/// `android.net.wifi.ScanResult` of one access point.
#[must_use]
pub fn scan_result_parcelable(ap: &AccessPoint, since_start_us: i64) -> Parcelable {
    Parcelable::ordered(
        "android.net.wifi.ScanResult",
        vec![
            PV::Cond {
                present: true,
                values: vec![PV::Parcelable(ssid_parcelable(&ap.ssid))],
            },
            PV::Str(Some(ap.ssid.clone())),
            PV::Str(Some(ap.bssid.clone())),
            PV::Long(0),  // hessid
            PV::Int(0),   // anqp domain id
            PV::Str(Some(ap.capabilities.clone())),
            PV::Int(ap.rssi),
            PV::Int(ap.frequency),
            PV::Long(since_start_us),
            PV::Int(-1),  // distanceCm
            PV::Int(-1),  // distanceSdCm
            PV::Int(0),   // channelWidth (20 MHz)
            PV::Int(0),   // centerFreq0
            PV::Int(0),   // centerFreq1
            PV::Int(4),   // wifi standard: 802.11n
            PV::Long(0),  // seen
            PV::Int(0),   // untrusted
            PV::Int(0),   // numUsage
            PV::Str(Some(String::new())),
            PV::Str(Some(String::new())),
            PV::Long(0),  // flags
            PV::List(Some(Vec::new())), // information elements
            PV::List(Some(Vec::new())), // anqp lines
            PV::List(Some(Vec::new())), // anqp elements
            PV::List(Some(Vec::new())), // radio chain infos
            PV::Str(Some("wlan0".to_string())),
            PV::NamedParcelable(Parcelable::Null),
            PV::Int(-1),
            PV::List(Some(Vec::new())),
        ],
    )
}

/// `android.net.DhcpInfo`: the session's address block when the `network`
/// module is attached (and the link is up), zeros otherwise.
#[must_use]
pub fn dhcp_info_parcelable(state: &WifiState) -> Parcelable {
    fn le(octets: [u8; 4]) -> i32 {
        i32::from_le_bytes(octets)
    }
    let values = if state.is_associated() && crate::network::session_has_network() {
        vec![
            le(crate::connectivity::NET_GUEST_ADDRESS),
            le(crate::connectivity::NET_GUEST_GATEWAY),
            le([255, 255, 255, 0]),
            le(crate::connectivity::NET_DNS_SERVER),
            0,
            le(crate::connectivity::NET_GUEST_GATEWAY),
            86_400,
        ]
    } else {
        vec![0; 7]
    };
    Parcelable::ordered(
        "android.net.DhcpInfo",
        values.into_iter().map(PV::Int).collect(),
    )
}

fn network_info(connected: bool) -> Parcelable {
    let state = if connected { "CONNECTED" } else { "DISCONNECTED" };
    Parcelable::ordered(
        "android.net.NetworkInfo",
        vec![
            PV::Int(crate::connectivity::LEGACY_TYPE_WIFI),
            PV::Int(0),
            PV::Str(Some("WIFI".to_string())),
            PV::Str(Some(String::new())),
            PV::Str(Some(state.to_string())),
            PV::Str(Some(state.to_string())),
            PV::Int(0),
            PV::Int(1),
            PV::Int(0),
            PV::Str(None),
            PV::Str(None),
        ],
    )
}

/// The default (associated, scan-visible) state, for tests that check what the
/// address-carrying parcelables say under a given `network` profile.
#[doc(hidden)]
#[must_use]
pub fn probe_state() -> WifiState {
    WifiState::new()
}

/// `WifiManager.calculateSignalLevel(rssi)`: the number of platform thresholds
/// the RSSI reaches (0..=4).
#[must_use]
pub fn signal_level(rssi: i32) -> i32 {
    i32::try_from(RSSI_THRESHOLDS.iter().filter(|t| rssi >= **t).count()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

const CONTROL_OPS: [ControlOp; 7] = [
    ControlOp {
        name: "state",
        args: "none — the radio, the associated access point, the scan list and the call counters",
    },
    ControlOp {
        name: "enable",
        args: "{on, autoconnect?} (scripted) — WIFI_STATE_CHANGED; off drops the connection (refused while the `network` module is `real`: use `acos_net offline`)",
    },
    ControlOp {
        name: "connect",
        args: "{ssid?, bssid?, rssi?, frequency?, link_speed?} (scripted) — associate: NETWORK_STATE_CHANGED (CONNECTED) and onAvailable on the default network",
    },
    ControlOp {
        name: "disconnect",
        args: "none (scripted) — NETWORK_STATE_CHANGED (DISCONNECTED) and onLost on the default network (refused while the `network` module is `real`)",
    },
    ControlOp {
        name: "rssi",
        args: "{rssi} (scripted) — change the associated RSSI; RSSI_CHANGED",
    },
    ControlOp {
        name: "scan",
        args: "{results?: [{ssid, bssid?, rssi?, frequency?, capabilities?}]} (scripted) — replace the scan list and complete a scan: SCAN_RESULTS_AVAILABLE and every IScanResultsCallback",
    },
    ControlOp {
        name: "reset",
        args: "none — clear the call counters and unsupported-call list",
    },
];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "Wi-Fi client radio: stub (one SSID from config) / scripted (connect, disconnect, rssi, scan -> WIFI_STATE_CHANGED, NETWORK_STATE_CHANGED, RSSI_CHANGED, SCAN_RESULTS, NetworkCallback loss/availability); no real radio, no hotspot/P2P/configured networks",
        services: &[ServiceProvide {
            name: SERVICE_NAME,
            descriptor: DESCRIPTOR,
        }],
        replaces: &[],
        process_initializers: &[],
        features: &["android.hardware.wifi"],
        providers: &[],
        broadcasts: &[
            ACTION_WIFI_STATE_CHANGED,
            ACTION_NETWORK_STATE_CHANGED,
            ACTION_RSSI_CHANGED,
            ACTION_SCAN_RESULTS,
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
pub struct WifiModule {
    profile: Profile,
    state: Option<Arc<Mutex<WifiState>>>,
}

impl WifiModule {
    /// A detached module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: Profile::Off,
            state: None,
        }
    }

    /// The manifest (tests and the registry).
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        manifest_ref()
    }
}

impl Default for WifiModule {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

struct WifiService {
    state: Arc<Mutex<WifiState>>,
    queued: Vec<PendingDelivery>,
    broadcasts: Vec<IntentFacts>,
}

impl WifiService {
    fn unsupported(&self, method: &str) -> ServiceError {
        lock(&self.state).unsupported.push(method.to_string());
        ServiceError::UnsupportedAndroidApi {
            interface: DESCRIPTOR.to_string(),
            code: 0,
            method: format!("{MODULE_ID}: {method} has no stub answer"),
        }
    }

    fn since_start_us(state: &WifiState) -> i64 {
        i64::try_from(state.started.elapsed().as_micros()).unwrap_or(i64::MAX)
    }

    fn write_bool(reply: &mut acos_parcel::Parcel, value: bool) {
        write_no_exception(reply);
        reply.write_int32(i32::from(value));
    }

    fn scan_completed_deliveries(state: &WifiState) -> Vec<PendingDelivery> {
        state
            .callbacks
            .iter()
            .filter_map(|callback| {
                let mut data = acos_parcel::Parcel::new();
                data.write_interface_token(SCAN_CALLBACK_DESCRIPTOR).ok()?;
                Some(PendingDelivery {
                    target: acos_parcel::BinderId(*callback),
                    interface: SCAN_CALLBACK_DESCRIPTOR.to_string(),
                    code: 1,
                    method: "onScanResultsAvailable".to_string(),
                    data,
                })
            })
            .collect()
    }

    fn scan_results_intent() -> IntentFacts {
        let mut intent = IntentFacts::action(ACTION_SCAN_RESULTS);
        intent
            .extras
            .insert("resultsUpdated".to_string(), Extra::Bool(true));
        intent
    }
}

impl ServiceObject for WifiService {
    fn drain_retained(&mut self) -> Vec<acos_parcel::BinderId> {
        lock(&self.state).refs.drain_retained()
    }

    fn drain_released(&mut self) -> Vec<acos_parcel::BinderId> {
        lock(&self.state).refs.drain_released()
    }

    fn release_all_refs(&mut self) -> Vec<acos_parcel::BinderId> {
        lock(&self.state).refs.release_all()
    }

    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<CallResult> {
        Err(self.unsupported(&format!("{interface}::{}", method.name)))
    }

    fn drain_raw_deliveries(&mut self) -> Vec<PendingDelivery> {
        std::mem::take(&mut self.queued)
    }

    fn drain_broadcasts(&mut self) -> Vec<IntentFacts> {
        std::mem::take(&mut self.broadcasts)
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        if interface != DESCRIPTOR {
            return Ok(false);
        }
        match code {
            // long getSupportedFeatures()
            1 => {
                write_no_exception(reply);
                reply.write_int64(1); // WIFI_FEATURE_INFRA
            }
            // boolean startScan(String packageName, String featureId)
            35 => {
                let (accepted, deliveries) = {
                    let mut state = lock(&self.state);
                    state.start_scan_calls += 1;
                    let accepted = state.enabled;
                    let deliveries = if accepted {
                        Self::scan_completed_deliveries(&state)
                    } else {
                        Vec::new()
                    };
                    (accepted, deliveries)
                };
                if accepted {
                    self.queued.extend(deliveries);
                    self.broadcasts.push(Self::scan_results_intent());
                }
                Self::write_bool(reply, accepted);
            }
            // List<ScanResult> getScanResults(String packageName, String featureId)
            36 => {
                let mut state = lock(&self.state);
                state.scan_result_calls += 1;
                if !state.scan_access {
                    return Err(ServiceError::android(
                        EX_SECURITY,
                        "getScanResults: ACCESS_FINE_LOCATION was not granted to this session \
                         (net.wifi config {\"scan_access\": true} grants it)",
                    ));
                }
                let since = Self::since_start_us(&state);
                let results: Vec<Parcelable> = if state.enabled {
                    state
                        .scan
                        .iter()
                        .map(|ap| scan_result_parcelable(ap, since))
                        .collect()
                } else {
                    Vec::new()
                };
                drop(state);
                write_no_exception(reply);
                reply.write_int32(i32::try_from(results.len()).unwrap_or(0));
                for result in &results {
                    reply.write_int32(1);
                    parcelable::write_payload(reply, result)?;
                }
            }
            // boolean disconnect/reconnect/reassociate(String packageName):
            // AOSP answers false to an app that is not the system or a
            // privileged caller (`WifiServiceImpl` checks NETWORK_SETTINGS /
            // the active connection owner), and ACOS has none of those.
            38..=40 => Self::write_bool(reply, false),
            // WifiInfo getConnectionInfo(String packageName, String featureId)
            41 => {
                let info = {
                    let mut state = lock(&self.state);
                    state.info_calls += 1;
                    wifi_info_parcelable(&state)
                };
                write_no_exception(reply);
                reply.write_int32(1);
                parcelable::write_payload(reply, &info)?;
            }
            // boolean setWifiEnabled(String packageName, boolean enable)
            42 => {
                lock(&self.state).set_enabled_calls += 1;
                Self::write_bool(reply, false);
            }
            // int getWifiEnabledState()
            43 => {
                let value = lock(&self.state).wifi_state();
                write_no_exception(reply);
                reply.write_int32(value);
            }
            // String getCountryCode(String packageName, String featureId)
            48 => {
                let country = lock(&self.state).country.clone();
                write_no_exception(reply);
                reply.write_string16_opt(Some(&country))?;
            }
            // boolean is24GHzBandSupported() .. is60GHzBandSupported()
            52..=55 => Self::write_bool(reply, code == 52),
            // boolean isWifiStandardSupported(int standard): legacy and 802.11n
            56 => {
                let standard = data.read_int32().unwrap_or(0);
                Self::write_bool(reply, standard == 1 || standard == 4);
            }
            // DhcpInfo getDhcpInfo(String packageName)
            57 => {
                let info = dhcp_info_parcelable(&lock(&self.state));
                write_no_exception(reply);
                reply.write_int32(1);
                parcelable::write_payload(reply, &info)?;
            }
            // boolean isScanAlwaysAvailable()
            59 => Self::write_bool(reply, false),
            // boolean acquireWifiLock(...) / void updateWifiLockWorkSource /
            // boolean releaseWifiLock(IBinder)
            60 => {
                lock(&self.state).lock_calls += 1;
                Self::write_bool(reply, true);
            }
            61 => write_no_exception(reply),
            62 => Self::write_bool(reply, true),
            // void initializeMulticastFiltering(); boolean isMulticastEnabled();
            // void acquireMulticastLock(IBinder, String); void releaseMulticastLock(String)
            63 => write_no_exception(reply),
            64 => Self::write_bool(reply, false),
            65 => {
                lock(&self.state).lock_calls += 1;
                write_no_exception(reply);
            }
            66 => write_no_exception(reply),
            // int getVerboseLoggingLevel()
            98 => {
                write_no_exception(reply);
                reply.write_int32(0);
            }
            // Network getCurrentNetwork()
            101 => {
                let up = {
                    let state = lock(&self.state);
                    state.is_associated() && state.network_up
                };
                write_no_exception(reply);
                if up {
                    reply.write_int32(1);
                    parcelable::write_payload(reply, &crate::connectivity::network_parcelable())?;
                } else {
                    reply.write_int32(0);
                }
            }
            // void registerScanResultsCallback(IScanResultsCallback)
            131 => {
                if let Ok(Some(callback)) = data.read_strong_binder() {
                    let mut state = lock(&self.state);
                    if !state.callbacks.contains(&callback.0) {
                        state.callbacks.push(callback.0);
                    }
                    state.refs.hold(callback);
                }
                write_no_exception(reply);
            }
            // void unregisterScanResultsCallback(IScanResultsCallback)
            132 => {
                if let Ok(Some(callback)) = data.read_strong_binder() {
                    let mut state = lock(&self.state);
                    state.callbacks.retain(|id| *id != callback.0);
                    state.refs.release(callback);
                }
                write_no_exception(reply);
            }
            // int calculateSignalLevel(int rssi)
            137 => {
                let rssi = data.read_int32().unwrap_or(INVALID_RSSI);
                write_no_exception(reply);
                reply.write_int32(signal_level(rssi));
            }
            _ => {
                // The host turns `Ok(false)` into the structured
                // `UNSUPPORTED_ANDROID_API` error; record which method asked.
                let name = acos_aidl::lookup(DESCRIPTOR, code)
                    .map_or_else(|| format!("code {code}"), |method| method.name.clone());
                lock(&self.state).unsupported.push(name);
                return Ok(false);
            }
        }
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Control helpers
// ---------------------------------------------------------------------------

fn arg_str(args: &Value, key: &str) -> Result<Option<String>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.is_empty() => Ok(Some(text.clone())),
        Some(other) => Err(rejected(format!(
            "`{key}` must be a non-empty string, got {other}"
        ))),
    }
}

fn arg_int(args: &Value, key: &str, min: i64, max: i64) -> Result<Option<i32>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let Some(number) = value.as_i64() else {
                return Err(rejected(format!("`{key}` must be an integer, got {value}")));
            };
            if number < min || number > max {
                return Err(rejected(format!(
                    "`{key}` {number} is outside {min}..={max}"
                )));
            }
            Ok(Some(i32::try_from(number).unwrap_or(0)))
        }
    }
}

fn check_bssid(text: &str) -> Result<()> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() != 6
        || !parts
            .iter()
            .all(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(rejected(format!(
            "BSSID {text:?} must be six colon-separated hex pairs (aa:bb:cc:dd:ee:ff)"
        )));
    }
    Ok(())
}

fn parse_access_points(value: &Value) -> Result<Vec<AccessPoint>> {
    let Some(items) = value.as_array() else {
        return Err(rejected(format!(
            "scan results must be an array of {{ssid, bssid?, rssi?, frequency?, capabilities?}}, got {value}"
        )));
    };
    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(ssid) = arg_str(item, "ssid")? else {
            return Err(rejected(format!("scan result {index} has no `ssid`")));
        };
        let bssid = match arg_str(item, "bssid")? {
            Some(bssid) => {
                check_bssid(&bssid)?;
                bssid
            }
            None => format!("02:00:00:00:{:02x}:{:02x}", index / 256, index % 256),
        };
        out.push(AccessPoint {
            ssid,
            bssid,
            rssi: arg_int(item, "rssi", -127, 0)?.unwrap_or(-60),
            frequency: arg_int(item, "frequency", 2000, 7200)?.unwrap_or(2437),
            capabilities: arg_str(item, "capabilities")?
                .unwrap_or_else(|| "[WPA2-PSK-CCMP][ESS]".to_string()),
        });
    }
    Ok(out)
}

fn apply_config(state: &mut WifiState, config: &ModuleConfig) -> Result<()> {
    let object = config.to_json();
    let Some(map) = object.as_object() else {
        return Ok(());
    };
    for key in map.keys() {
        if !CONFIG_KEYS.contains(&key.as_str()) {
            return Err(rejected(format!(
                "unknown config key {key:?}; this module takes {} (the initial state is \
                 connected: script a lost network with the `disconnect` control op)",
                CONFIG_KEYS.join(", ")
            )));
        }
    }
    let value = Value::Object(map.clone());
    if let Some(ssid) = arg_str(&value, "ssid")? {
        state.ssid = ssid;
    }
    if let Some(bssid) = arg_str(&value, "bssid")? {
        check_bssid(&bssid)?;
        state.bssid = bssid;
    }
    if let Some(rssi) = arg_int(&value, "rssi", -127, 0)? {
        state.rssi = rssi;
    }
    if let Some(frequency) = arg_int(&value, "frequency", 2000, 7200)? {
        state.frequency = frequency;
    }
    if let Some(link) = arg_int(&value, "link_speed", 1, 10_000)? {
        state.link_speed = link;
    }
    if let Some(capabilities) = arg_str(&value, "capabilities")? {
        state.capabilities = capabilities;
    }
    if let Some(country) = arg_str(&value, "country")? {
        state.country = country;
    }
    match map.get("scan_access") {
        None => {}
        Some(Value::Bool(flag)) => state.scan_access = *flag,
        Some(other) => {
            return Err(rejected(format!(
                "`scan_access` must be a boolean, got {other}"
            )))
        }
    }
    match map.get("enabled") {
        None => {}
        Some(Value::Bool(flag)) => state.enabled = *flag,
        Some(other) => {
            return Err(rejected(format!("`enabled` must be a boolean, got {other}")))
        }
    }
    match map.get("networks") {
        Some(networks) => state.scan = parse_access_points(networks)?,
        None => {
            // The default scan list is the associated network itself, so a
            // configured SSID is also what a scan finds.
            state.scan = vec![AccessPoint {
                ssid: state.ssid.clone(),
                bssid: state.bssid.clone(),
                rssi: state.rssi,
                frequency: state.frequency,
                capabilities: state.capabilities.clone(),
            }];
        }
    }
    Ok(())
}

impl Module for WifiModule {
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
                "{profile} is not implemented: ACOS has no Wi-Fi radio, so use `stub` or `scripted`"
            )));
        }
        let mut state = WifiState::new();
        apply_config(&mut state, config)?;
        let state = Arc::new(Mutex::new(state));
        host.register_dynamic_named(
            SERVICE_NAME,
            DESCRIPTOR,
            Box::new(WifiService {
                state: Arc::clone(&state),
                queued: Vec::new(),
                broadcasts: Vec::new(),
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
            return Err(rejected(format!(
                "module is off, so {op:?} was not applied"
            )));
        };
        match op {
            "state" => return Ok(lock(&shared).to_json()),
            "reset" => {
                let mut state = lock(&shared);
                state.info_calls = 0;
                state.scan_result_calls = 0;
                state.start_scan_calls = 0;
                state.set_enabled_calls = 0;
                state.lock_calls = 0;
                state.unsupported.clear();
                return Ok(json!({"reset": true}));
            }
            "enable" | "connect" | "disconnect" | "rssi" | "scan" => {}
            other => {
                return Err(rejected(format!(
                    "unknown control op {other:?}; this module implements {}",
                    CONTROL_OPS
                        .iter()
                        .map(|op| op.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                )))
            }
        }
        if self.profile != Profile::Scripted {
            return Err(rejected(format!(
                "{op:?} changes the radio, and the `stub` profile answers fixed values; \
                 attach `scripted` ({{\"id\": \"{MODULE_ID}\", \"profile\": \"scripted\"}}) to drive it"
            )));
        }
        match op {
            "enable" => control_enable(host, &shared, args),
            "connect" => control_connect(host, &shared, args),
            "disconnect" => control_disconnect(host, &shared),
            "rssi" => control_rssi(host, &shared, args),
            _ => control_scan(host, &shared, args),
        }
    }

    fn report(&self) -> ModuleReport {
        let mut report = ModuleReport::of(manifest_ref(), self.profile);
        if self.profile.is_attached() {
            let (enabled, connected, ssid) = self
                .state
                .as_ref()
                .map(|state| {
                    let state = lock(state);
                    (state.enabled, state.is_associated(), state.ssid.clone())
                })
                .unwrap_or_default();
            report.reason = Some(format!(
                "{} profile: scripted client radio (Wi-Fi {}, {}), no real radio — scan results, RSSI and association are values the script set, not measurements; no hotspot, P2P, configured networks or suggestions (UNSUPPORTED_ANDROID_API); DhcpInfo/WifiInfo address are zero/absent unless the `network` module is attached",
                self.profile,
                if enabled { "on" } else { "off" },
                if connected {
                    format!("associated with {ssid:?}")
                } else {
                    "not associated".to_string()
                },
            ));
        }
        report
    }
}

/// Move the session's default network and queue the callbacks the move owes.
/// Returns how many `NetworkCallback` messages were queued.
fn move_default_network(host: &mut ServiceHost, up: bool) -> usize {
    let Some(connectivity) = host.connectivity_state() else {
        return 0;
    };
    let deliveries = connectivity
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .set_default_network(up);
    let count = deliveries.len();
    for delivery in deliveries {
        host.queue_delivery(delivery);
    }
    count
}

/// A script may not drop a link the `network` module's relay still carries:
/// the callbacks would say "lost" while sockets kept working.
fn refuse_while_networked(what: &str) -> Result<()> {
    if crate::network::networked() {
        return Err(rejected(format!(
            "{what} would drop the Wi-Fi link while the `network` module is `real` and its \
             relay keeps carrying traffic (the callbacks would say \"lost\" while sockets work). \
             Use `acos_net offline` (it drops the packets and the default network together)"
        )));
    }
    Ok(())
}

fn wifi_state_broadcast(host: &mut ServiceHost, now: i32, previous: i32) -> Result<i32> {
    let mut intent = IntentFacts::action(ACTION_WIFI_STATE_CHANGED);
    intent
        .extras
        .insert("wifi_state".to_string(), Extra::Int(now));
    intent
        .extras
        .insert("previous_wifi_state".to_string(), Extra::Int(previous));
    host.broadcast_intent(&intent)
}

fn network_state_broadcast(
    host: &mut ServiceHost,
    shared: &Arc<Mutex<WifiState>>,
    connected: bool,
) -> Result<i32> {
    let (info, bssid) = {
        let state = lock(shared);
        (wifi_info_parcelable(&state), state.bssid.clone())
    };
    let mut intent = IntentFacts::action(ACTION_NETWORK_STATE_CHANGED);
    intent.extras.insert(
        "networkInfo".to_string(),
        Extra::Parcelable(network_info(connected)),
    );
    if connected {
        intent
            .extras
            .insert("bssid".to_string(), Extra::String(bssid));
        intent
            .extras
            .insert("wifiInfo".to_string(), Extra::Parcelable(info));
    }
    host.broadcast_intent(&intent)
}

fn control_enable(
    host: &mut ServiceHost,
    shared: &Arc<Mutex<WifiState>>,
    args: &Value,
) -> Result<Value> {
    let on = match args.get("on") {
        Some(Value::Bool(on)) => *on,
        _ => return Err(rejected("enable needs {on: true|false}")),
    };
    let autoconnect = match args.get("autoconnect") {
        None => true,
        Some(Value::Bool(flag)) => *flag,
        Some(other) => {
            return Err(rejected(format!(
                "`autoconnect` must be a boolean, got {other}"
            )))
        }
    };
    let (was_enabled, was_connected) = {
        let state = lock(shared);
        (state.enabled, state.is_associated())
    };
    if was_enabled == on {
        return Ok(json!({
            "enabled": on,
            "changed": false,
            "broadcast_matched": 0,
            "callbacks_queued": 0,
        }));
    }
    if !on && was_connected {
        refuse_while_networked("enable {on: false}")?;
    }
    let mut matched = 0;
    let mut callbacks = 0;
    if on {
        matched += wifi_state_broadcast(host, WIFI_STATE_ENABLING, WIFI_STATE_DISABLED)?;
        lock(shared).enabled = true;
        matched += wifi_state_broadcast(host, WIFI_STATE_ENABLED, WIFI_STATE_ENABLING)?;
        if autoconnect && lock(shared).connected {
            let up = !crate::network::offline();
            lock(shared).network_up = up;
            callbacks += move_default_network(host, up);
            matched += network_state_broadcast(host, shared, true)?;
        }
    } else {
        matched += wifi_state_broadcast(host, WIFI_STATE_DISABLING, WIFI_STATE_ENABLED)?;
        {
            let mut state = lock(shared);
            state.enabled = false;
            state.network_up = false;
        }
        if was_connected {
            callbacks += move_default_network(host, false);
            matched += network_state_broadcast(host, shared, false)?;
        }
        matched += wifi_state_broadcast(host, WIFI_STATE_DISABLED, WIFI_STATE_DISABLING)?;
    }
    Ok(json!({
        "enabled": on,
        "changed": true,
        "wifi_state": lock(shared).wifi_state(),
        "broadcast_matched": matched,
        "callbacks_queued": callbacks,
    }))
}

fn control_connect(
    host: &mut ServiceHost,
    shared: &Arc<Mutex<WifiState>>,
    args: &Value,
) -> Result<Value> {
    if !lock(shared).enabled {
        return Err(rejected(
            "connect: Wi-Fi is off; `enable {on: true}` first (a radio that is off cannot associate)",
        ));
    }
    let ssid = arg_str(args, "ssid")?;
    let bssid = arg_str(args, "bssid")?;
    if let Some(bssid) = &bssid {
        check_bssid(bssid)?;
    }
    let rssi = arg_int(args, "rssi", -127, 0)?;
    let frequency = arg_int(args, "frequency", 2000, 7200)?;
    let link_speed = arg_int(args, "link_speed", 1, 10_000)?;
    {
        let mut state = lock(shared);
        if let Some(ssid) = ssid {
            state.ssid = ssid;
        }
        if let Some(bssid) = bssid {
            state.bssid = bssid;
        }
        if let Some(rssi) = rssi {
            state.rssi = rssi;
        }
        if let Some(frequency) = frequency {
            state.frequency = frequency;
        }
        if let Some(link_speed) = link_speed {
            state.link_speed = link_speed;
        }
        state.connected = true;
        let (ssid, bssid, rssi, frequency, capabilities) = (
            state.ssid.clone(),
            state.bssid.clone(),
            state.rssi,
            state.frequency,
            state.capabilities.clone(),
        );
        // A network the script connects to is one a scan finds.
        if !state.scan.iter().any(|ap| ap.ssid == ssid && ap.bssid == bssid) {
            state.scan.push(AccessPoint {
                ssid,
                bssid,
                rssi,
                frequency,
                capabilities,
            });
        }
    }
    // The `network` module's own `offline` control keeps the default network
    // down no matter what Wi-Fi says (no fake connectivity).
    let up = !crate::network::offline();
    lock(shared).network_up = up;
    let callbacks = move_default_network(host, up);
    let matched = network_state_broadcast(host, shared, true)?;
    let state = lock(shared);
    Ok(json!({
        "connected": true,
        "ssid": state.ssid,
        "bssid": state.bssid,
        "default_network_up": up,
        "broadcast_matched": matched,
        "callbacks_queued": callbacks,
    }))
}

fn control_disconnect(host: &mut ServiceHost, shared: &Arc<Mutex<WifiState>>) -> Result<Value> {
    let was_connected = lock(shared).is_associated();
    if !was_connected {
        return Ok(json!({
            "connected": false,
            "changed": false,
            "broadcast_matched": 0,
            "callbacks_queued": 0,
        }));
    }
    refuse_while_networked("disconnect")?;
    {
        let mut state = lock(shared);
        state.connected = false;
        state.network_up = false;
    }
    let callbacks = move_default_network(host, false);
    let matched = network_state_broadcast(host, shared, false)?;
    Ok(json!({
        "connected": false,
        "changed": true,
        "default_network_up": false,
        "broadcast_matched": matched,
        "callbacks_queued": callbacks,
    }))
}

fn control_rssi(
    host: &mut ServiceHost,
    shared: &Arc<Mutex<WifiState>>,
    args: &Value,
) -> Result<Value> {
    let Some(rssi) = arg_int(args, "rssi", -127, 0)? else {
        return Err(rejected("rssi needs {rssi: <dBm, -127..0>}"));
    };
    if !lock(shared).is_associated() {
        return Err(rejected(
            "rssi: Wi-Fi is not associated (AOSP sends RSSI_CHANGED only for a connected link)",
        ));
    }
    {
        let mut state = lock(shared);
        state.rssi = rssi;
        let (ssid, bssid) = (state.ssid.clone(), state.bssid.clone());
        for ap in &mut state.scan {
            if ap.ssid == ssid && ap.bssid == bssid {
                ap.rssi = rssi;
            }
        }
    }
    let mut intent = IntentFacts::action(ACTION_RSSI_CHANGED);
    intent
        .extras
        .insert("newRssi".to_string(), Extra::Int(rssi));
    let matched = host.broadcast_intent(&intent)?;
    Ok(json!({
        "rssi": rssi,
        "level": signal_level(rssi),
        "broadcast_matched": matched,
    }))
}

fn control_scan(
    host: &mut ServiceHost,
    shared: &Arc<Mutex<WifiState>>,
    args: &Value,
) -> Result<Value> {
    if !lock(shared).enabled {
        return Err(rejected(
            "scan: Wi-Fi is off; AOSP completes no scan and broadcasts no results while the radio is off",
        ));
    }
    if let Some(results) = args.get("results") {
        let parsed = parse_access_points(results)?;
        lock(shared).scan = parsed;
    }
    let (deliveries, count) = {
        let state = lock(shared);
        (WifiService::scan_completed_deliveries(&state), state.scan.len())
    };
    let callbacks = deliveries.len();
    for delivery in deliveries {
        host.queue_delivery(delivery);
    }
    let matched = host.broadcast_intent(&WifiService::scan_results_intent())?;
    Ok(json!({
        "results": count,
        "broadcast_matched": matched,
        "callbacks_queued": callbacks,
    }))
}
