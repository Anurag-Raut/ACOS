//! `IConnectivityManager` — the **default-network** state machine (NET1).
//!
//! React Native's `@react-native-community/netinfo` reaches
//! `ConnectivityManager.registerNetworkCallback` on every launch, which is
//! `IConnectivityManager.requestNetwork(asUid, caps, TRACK_DEFAULT.ordinal(),
//! messenger, timeout, binder, legacyType, callbackFlags, pkg, tag)` (transaction
//! **42** in the API-34 image; verified against
//! `packages/modules/Connectivity/framework/src/android/net/IConnectivityManager.aidl`
//! where `requestNetwork` is the 42nd method, and against the decoded
//! `acos-aidl` table for the same descriptor). Before NET1 that transaction was
//! a structured `UnsupportedAndroidApi`, which the app's JNI layer rethrows as
//! `JniException` and React Native aborts the process on.
//!
//! # What ACOS models
//!
//! ACOS's sandbox runs `--unshare-net`: the guest has no interface and no
//! sockets. Rather than answer every network question with `null` (which makes
//! every RN app abort), the service models **one emulated default network** —
//! AOSP's own model is "a set of `Network`s, each with `NetworkCapabilities` and
//! `LinkProperties`", and the app-facing surface is the callback protocol. The
//! network is *declared*, not discovered:
//!
//! * id [`NET_ID`], legacy type `TYPE_WIFI`, transport `WIFI`;
//! * capabilities `INTERNET`, `NOT_METERED`, `NOT_RESTRICTED`, `TRUSTED`,
//!   `NOT_VPN`, `NOT_ROAMING`, `NOT_CONGESTED`, `NOT_SUSPENDED`, `FOREGROUND`,
//!   `NOT_VCN_MANAGED`;
//! * **no** `NET_CAPABILITY_VALIDATED`: AOSP sets it only after a captive-portal
//!   probe reached the internet, and ACOS cannot probe (and does not claim)
//!   reachability. Apps therefore see `isConnected` true and
//!   `isInternetReachable` false — the honest pair for a sandbox with no
//!   sockets;
//! * an empty `LinkProperties` address list (there is no interface to describe),
//!   with the interface name and MTU filled in so `getInterfaceName` answers.
//!
//! # Callback protocol (AOSP `ConnectivityService.callCallbackForRequest`)
//!
//! Registration on a network that already matches delivers, in this order, to
//! the app's `Messenger`:
//!
//! 1. `CALLBACK_AVAILABLE` (2) — `arg1` = blocked status; bundle carries
//!    `NetworkRequest`, `Network`, `NetworkCapabilities`, `LinkProperties`,
//!    `LocalNetworkInfo` (null);
//! 2. `CALLBACK_CAP_CHANGED` (6) — `NetworkRequest`, `Network`,
//!    `NetworkCapabilities`;
//! 3. `CALLBACK_IP_CHANGED` (7) — `NetworkRequest`, `Network`,
//!    `LinkProperties`;
//! 4. `CALLBACK_BLK_CHANGED` (11) — `arg1` = 0, `NetworkRequest`, `Network`.
//!
//! Each message is one `android.os.IMessenger.send(Message)` transaction
//! (interface `android.os.IMessenger`, code 1, oneway in the AIDL) whose body is
//! the framework's own `Message.writeToParcel` layout. The bundle is written by
//! [`acos_services::parcelable::write_parcelable_bundle`] because
//! `Parcel.writeValue`'s `VAL_PARCELABLE` case is a class name plus the payload,
//! with no length word.
//!
//! `releaseNetworkRequest`/`unregisterNetworkCallback` drop the request; a
//! second release of an unknown id is AOSP's `IllegalArgumentException` and is
//! reported as a structured error here, never a silent success.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use acos_aidl::MethodInfo;
use acos_parcel::{BinderId, Parcel};

use crate::error::{ServiceError, Result};
use crate::host::{CallResult, PendingDelivery, ServiceObject};
use crate::parcelable;
use crate::value::{Parcelable, Value};

/// The emulated default network's id (`ConnectivityService` hands these out
/// from 100 on a device).
pub const NET_ID: i32 = 100;

/// `ConnectivityManager.TYPE_WIFI` (the legacy type of the emulated network).
pub const LEGACY_TYPE_WIFI: i32 = 1;

/// `ConnectivityManager.TYPE_NONE`.
pub const LEGACY_TYPE_NONE: i32 = -1;

/// `NetworkCapabilities.TRANSPORT_WIFI`.
pub const TRANSPORT_WIFI: i32 = 1;

/// `NetworkCapabilities.NET_CAPABILITY_*` bit positions (API 34).
pub const CAP_NOT_METERED: u32 = 11;
/// `NET_CAPABILITY_INTERNET`.
pub const CAP_INTERNET: u32 = 12;
/// `NET_CAPABILITY_NOT_RESTRICTED`.
pub const CAP_NOT_RESTRICTED: u32 = 13;
/// `NET_CAPABILITY_TRUSTED`.
pub const CAP_TRUSTED: u32 = 14;
/// `NET_CAPABILITY_NOT_VPN`.
pub const CAP_NOT_VPN: u32 = 15;
/// `NET_CAPABILITY_VALIDATED` (deliberately **not** set, see the module docs).
pub const CAP_VALIDATED: u32 = 16;
/// `NET_CAPABILITY_NOT_ROAMING`.
pub const CAP_NOT_ROAMING: u32 = 18;
/// `NET_CAPABILITY_FOREGROUND`.
pub const CAP_FOREGROUND: u32 = 19;
/// `NET_CAPABILITY_NOT_CONGESTED`.
pub const CAP_NOT_CONGESTED: u32 = 20;
/// `NET_CAPABILITY_NOT_SUSPENDED`.
pub const CAP_NOT_SUSPENDED: u32 = 21;
/// `NET_CAPABILITY_NOT_VCN_MANAGED`.
pub const CAP_NOT_VCN_MANAGED: u32 = 28;

/// `ConnectivityManager.CALLBACK_AVAILABLE`.
pub const CALLBACK_AVAILABLE: i32 = 2;
/// `ConnectivityManager.CALLBACK_CAP_CHANGED`.
pub const CALLBACK_CAP_CHANGED: i32 = 6;
/// `ConnectivityManager.CALLBACK_IP_CHANGED`.
pub const CALLBACK_IP_CHANGED: i32 = 7;
/// `ConnectivityManager.CALLBACK_BLK_CHANGED`.
pub const CALLBACK_BLK_CHANGED: i32 = 11;

/// `android.os.IMessenger`'s only transaction (`send`, `oneway`).
pub const IMESSENGER_SEND: u32 = 1;

/// The capabilities bitfield of the emulated default network.
#[must_use]
pub fn default_capabilities_bits() -> i64 {
    let mut bits = 0i64;
    for cap in [
        CAP_NOT_METERED,
        CAP_INTERNET,
        CAP_NOT_RESTRICTED,
        CAP_TRUSTED,
        CAP_NOT_VPN,
        CAP_NOT_ROAMING,
        CAP_FOREGROUND,
        CAP_NOT_CONGESTED,
        CAP_NOT_SUSPENDED,
        CAP_NOT_VCN_MANAGED,
    ] {
        bits |= 1i64 << cap;
    }
    bits
}

/// `NetworkRequest.Type` ordinals (`ConnectivityManager.sendRequestForNetwork`
/// sends `reqType.ordinal()`), in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestType {
    /// `NONE` — the system default request.
    None,
    /// `LISTEN` — `registerNetworkCallback`.
    Listen,
    /// `TRACK_DEFAULT` — `registerDefaultNetworkCallback`.
    TrackDefault,
    /// `REQUEST` — `requestNetwork(NetworkRequest, NetworkCallback)`.
    Request,
    /// `BACKGROUND_REQUEST`.
    BackgroundRequest,
    /// `TRACK_SYSTEM_DEFAULT`.
    TrackSystemDefault,
    /// `LISTEN_FOR_BEST`.
    ListenForBest,
}

impl RequestType {
    /// Decode `Type.ordinal()` as it arrives on the wire.
    #[must_use]
    pub fn from_ordinal(ordinal: i32) -> Option<Self> {
        Some(match ordinal {
            0 => Self::None,
            1 => Self::Listen,
            2 => Self::TrackDefault,
            3 => Self::Request,
            4 => Self::BackgroundRequest,
            5 => Self::TrackSystemDefault,
            6 => Self::ListenForBest,
            _ => return None,
        })
    }

    /// `Type.name()`, which `NetworkRequest.CREATOR` feeds to `Type.valueOf`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::Listen => "LISTEN",
            Self::TrackDefault => "TRACK_DEFAULT",
            Self::Request => "REQUEST",
            Self::BackgroundRequest => "BACKGROUND_REQUEST",
            Self::TrackSystemDefault => "TRACK_SYSTEM_DEFAULT",
            Self::ListenForBest => "LISTEN_FOR_BEST",
        }
    }

    /// `Type.isListen()`.
    #[must_use]
    pub fn is_listen(self) -> bool {
        matches!(self, Self::Listen)
    }
}

/// One registered request.
#[derive(Debug, Clone)]
pub struct RequestRecord {
    /// The `NetworkRequest`'s id (the value ACOS put on the wire).
    pub request_id: i32,
    /// The request kind.
    pub kind: RequestType,
    /// The legacy type the caller asked for (`TYPE_NONE` usually).
    pub legacy_type: i32,
    /// `NetworkCallback.mFlags` (`FLAG_INCLUDE_LOCATION_INFO`).
    pub callback_flags: i32,
    /// The app's `Messenger` binder (delivery target).
    pub messenger: Option<BinderId>,
    /// The reply parcelable this request was answered with (echoed in every
    /// callback message, so `NetworkRequest.equals` — the app's `sCallbacks`
    /// key — holds).
    pub request: Parcelable,
    /// The requesting uid as sent by the app (`Process.INVALID_UID` for
    /// non-`REQUEST` types).
    pub uid: i32,
    /// `Context.getOpPackageName()` of the caller.
    pub package: String,
}

/// `IConnectivityManager` state.
#[derive(Debug, Default)]
pub struct ConnectivityState {
    /// Recorded proxy lookups (`Network` presence marker).
    pub proxy_lookups: u32,
    /// Live requests, in registration order.
    pub requests: BTreeMap<i32, RequestRecord>,
    /// Callback messages sent (evidence counter).
    pub callbacks_sent: u64,
    /// Requests released (evidence counter).
    pub released: u64,
    next_request_id: i32,
}

impl ConnectivityState {
    /// New empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_request_id: 1,
            ..Self::default()
        }
    }

    fn allocate_id(&mut self) -> i32 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        id
    }
}

/// `IConnectivityManager` handler.
pub struct ConnectivityService {
    state: Arc<Mutex<ConnectivityState>>,
}

impl ConnectivityService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<ConnectivityState>>) -> Self {
        Self { state }
    }
}

// ---------------------------------------------------------------------------
// Parcelable builders (AOSP `NetworkCapabilities`/`LinkProperties`/`Network`)
// ---------------------------------------------------------------------------

/// `android.net.Network` for the emulated default network.
#[must_use]
pub fn network_parcelable() -> Parcelable {
    Parcelable::ordered("android.net.Network", vec![Value::Int(NET_ID)])
}

/// `android.net.NetworkCapabilities` of the emulated default network.
#[must_use]
pub fn default_network_capabilities() -> Parcelable {
    Parcelable::ordered(
        "android.net.NetworkCapabilities",
        vec![
            Value::Long(default_capabilities_bits()),
            Value::Long(0),                                      // forbidden
            Value::Long(1i64 << TRANSPORT_WIFI),                 // transports
            Value::Int(0),                                       // link up kbps
            Value::Int(0),                                       // link down kbps
            Value::Str(None),                                    // NetworkSpecifier
            Value::Str(None),                                    // TransportInfo
            Value::Int(i32::MIN),                                // SIGNAL_STRENGTH_UNSPECIFIED
            Value::Int(-1),                                      // mUids (null ArraySet)
            Value::IntArray(Some(Vec::new())),                   // mAllowedUids
            Value::Str(None),                                    // mSSID
            Value::Bool(false),                                  // mPrivateDnsBroken
            Value::IntArray(Some(Vec::new())),                   // administrator uids
            Value::Int(1000),                                    // owner uid (system)
            Value::Int(1000),                                    // requestor uid
            Value::Str(Some("android".to_string())),             // requestor package
            Value::IntArray(Some(Vec::new())),                   // mSubIds
            Value::Int(-1),                                      // underlying networks (null)
            Value::Int(0),                                       // enterprise ids
        ],
    )
}

/// `android.net.LinkProperties` of the emulated default network.
///
/// There is no interface in the sandbox, so the three address lists, the route
/// list and the stacked-link list are explicit `0` counts. The interface name
/// and MTU are the AOSP emulator's wifi defaults, so `getInterfaceName()` and
/// `getMtu()` answer with a plausible, documented value instead of `null`.
#[must_use]
pub fn default_link_properties() -> Parcelable {
    Parcelable::ordered(
        "android.net.LinkProperties",
        vec![
            Value::Str(Some("wlan0".to_string())),
            Value::Int(0),  // link addresses
            Value::Int(0),  // DNS servers
            Value::Int(0),  // validated private DNS servers
            Value::Bool(false), // usePrivateDns
            Value::Str(None),   // privateDnsServerName
            Value::Int(0),      // PCSCF servers
            Value::Str(None),   // domains
            Value::ByteArray(None), // dhcp server address
            Value::Int(1500),   // mtu
            Value::Str(None),   // tcpBufferSizes
            Value::Int(0),      // routes
            Value::Byte(0),     // httpProxy presence
            Value::Str(None),   // nat64Prefix
            Value::Int(0),      // stacked links
            Value::Bool(false), // wakeOnLanSupported
            Value::Str(None),   // captivePortalApiUrl
            Value::Str(None),   // captivePortalData
        ],
    )
}

/// `android.net.NetworkInfo` (the deprecated reply) for the default network.
#[must_use]
pub fn default_network_info() -> Parcelable {
    Parcelable::ordered(
        "android.net.NetworkInfo",
        vec![
            Value::Int(LEGACY_TYPE_WIFI),
            Value::Int(0),
            Value::Str(Some("WIFI".to_string())),
            Value::Str(Some("".to_string())),
            Value::Str(Some("CONNECTED".to_string())),
            Value::Str(Some("CONNECTED".to_string())),
            Value::Int(0), // mIsFailover
            Value::Int(1), // mIsAvailable
            Value::Int(0), // mIsRoaming
            Value::Str(None),
            Value::Str(None),
        ],
    )
}

/// `android.net.NetworkRequest` with the capabilities payload `nc`.
#[must_use]
pub fn network_request(
    nc: Parcelable,
    legacy_type: i32,
    request_id: i32,
    kind: RequestType,
) -> Parcelable {
    Parcelable::ordered(
        "android.net.NetworkRequest",
        vec![
            Value::Parcelable(nc),
            Value::Int(legacy_type),
            Value::Int(request_id),
            Value::Str(Some(kind.name().to_string())),
        ],
    )
}

// ---------------------------------------------------------------------------
// Message delivery
// ---------------------------------------------------------------------------

/// Encode `android.os.Message.writeToParcel` for one callback message.
///
/// The layout is `what`, `arg1`, `arg2`, the `obj` presence marker (always 0
/// here), `when`, the data bundle, the `replyTo` messenger (a null strong
/// binder is a single `0` word, not a flat object) and the two uid words.
#[must_use]
pub fn encode_message(
    what: i32,
    arg1: i32,
    entries: &[(&str, &Parcelable)],
) -> std::result::Result<Parcel, String> {
    let mut parcel = Parcel::new();
    parcel
        .write_interface_token("android.os.IMessenger")
        .map_err(|e| format!("IMessenger token: {e}"))?;
    // `in Message msg`: AIDL's nullable-payload presence marker.
    parcel.write_int32(1);
    parcel.write_int32(what);
    parcel.write_int32(arg1);
    parcel.write_int32(0); // arg2
    parcel.write_int32(0); // obj == null
    parcel.write_int64(0); // when
    parcelable::write_parcelable_bundle(&mut parcel, entries)
        .map_err(|e| format!("Message data bundle: {e}"))?;
    // `Messenger.writeMessengerOrNullToParcel(replyTo, dest)` with a null
    // messenger is `writeStrongBinder(null)` == one `0` word.
    parcel.write_int32(0);
    parcel.write_int32(0); // sendingUid
    parcel.write_int32(0); // workSourceUid
    parcel.set_data_position(0);
    Ok(parcel)
}

/// The four callback messages AOSP sends for a request on a live network.
///
/// `request` is the reply parcelable the registration was answered with.
#[must_use]
pub fn callback_messages(request: &Parcelable) -> Vec<(i32, i32, Parcel)> {
    let network = network_parcelable();
    let caps = default_network_capabilities();
    let link = default_link_properties();
    let local = Parcelable::Null;
    let req = request.clone();
    let mut out = Vec::with_capacity(4);

    // 1. CALLBACK_AVAILABLE: `arg1` is the blocked status
    //    (`BLOCKED_REASON_NONE` for an unmetered, unblocked default network).
    out.push((
        CALLBACK_AVAILABLE,
        0,
        encode_message(
            CALLBACK_AVAILABLE,
            0,
            &[
                ("NetworkRequest", &req),
                ("Network", &network),
                ("NetworkCapabilities", &caps),
                ("LinkProperties", &link),
                ("LocalNetworkInfo", &local),
            ],
        )
        .expect("callback bundle"),
    ));
    // 2. CALLBACK_CAP_CHANGED.
    out.push((
        CALLBACK_CAP_CHANGED,
        0,
        encode_message(
            CALLBACK_CAP_CHANGED,
            0,
            &[
                ("NetworkRequest", &req),
                ("Network", &network),
                ("NetworkCapabilities", &caps),
            ],
        )
        .expect("callback bundle"),
    ));
    // 3. CALLBACK_IP_CHANGED.
    out.push((
        CALLBACK_IP_CHANGED,
        0,
        encode_message(
            CALLBACK_IP_CHANGED,
            0,
            &[
                ("NetworkRequest", &req),
                ("Network", &network),
                ("LinkProperties", &link),
            ],
        )
        .expect("callback bundle"),
    ));
    // 4. CALLBACK_BLK_CHANGED (`arg1` = blocked reasons).
    out.push((
        CALLBACK_BLK_CHANGED,
        0,
        encode_message(
            CALLBACK_BLK_CHANGED,
            0,
            &[("NetworkRequest", &req), ("Network", &network)],
        )
        .expect("callback bundle"),
    ));
    out
}

fn messenger_binder(value: Option<&Value>) -> Option<BinderId> {
    match value {
        Some(Value::Parcelable(Parcelable::Ordered { class, values }))
        | Some(Value::TypedObject(Parcelable::Ordered { class, values }))
            if class == "android.os.Messenger" =>
        {
            values.first().and_then(Value::as_binder)
        }
        _ => None,
    }
}

fn capabilities_of(value: Option<&Value>) -> Result<Parcelable> {
    match value {
        Some(Value::Parcelable(p @ Parcelable::Ordered { .. }))
        | Some(Value::TypedObject(p @ Parcelable::Ordered { .. })) => Ok(p.clone()),
        Some(Value::Parcelable(Parcelable::Null)) | Some(Value::TypedObject(Parcelable::Null)) => {
            Ok(Parcelable::Null)
        }
        other => Err(ServiceError::BadRequest {
            interface: "android.net.IConnectivityManager".into(),
            method: "requestNetwork".into(),
            detail: format!("NetworkCapabilities argument is {other:?}"),
        }),
    }
}

impl ConnectivityService {
    /// Register a request and return its reply plus queued callback messages.
    fn register(
        &self,
        state: &mut ConnectivityState,
        kind: RequestType,
        legacy_type: i32,
        callback_flags: i32,
        capabilities: Parcelable,
        messenger: Option<BinderId>,
        uid: i32,
        package: String,
    ) -> (Parcelable, Vec<PendingDelivery>) {
        let request_id = state.allocate_id();
        let reply = network_request(capabilities, legacy_type, request_id, kind);
        let mut deliveries = Vec::new();
        if let Some(target) = messenger {
            // AOSP `ConnectivityService.callCallbackForRequest` returns early
            // when the request has no messenger (the system default request),
            // and ACOS follows it: no messenger, no callback messages.
            for (code, _arg1, data) in callback_messages(&reply) {
                let method = match code {
                    CALLBACK_AVAILABLE => "CALLBACK_AVAILABLE",
                    CALLBACK_CAP_CHANGED => "CALLBACK_CAP_CHANGED",
                    CALLBACK_IP_CHANGED => "CALLBACK_IP_CHANGED",
                    _ => "CALLBACK_BLK_CHANGED",
                };
                deliveries.push(PendingDelivery {
                    target,
                    interface: "android.os.IMessenger".to_string(),
                    code: IMESSENGER_SEND,
                    method: method.to_string(),
                    data,
                });
            }
            state.callbacks_sent += deliveries.len() as u64;
        }
        state.requests.insert(
            request_id,
            RequestRecord {
                request_id,
                kind,
                legacy_type,
                callback_flags,
                messenger,
                request: reply.clone(),
                uid,
                package,
            },
        );
        (reply, deliveries)
    }
}

impl ServiceObject for ConnectivityService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.net.IConnectivityManager" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let mut state = self.state.lock().unwrap();
        let mut deliveries = Vec::new();
        let reply = match method.name.as_str() {
            // ---- network queries -------------------------------------------
            "getActiveNetwork" | "getActiveNetworkForUid" => vec![Value::Parcelable(network_parcelable())],
            "getActiveNetworkInfo" | "getActiveNetworkInfoForUid" => {
                vec![Value::Parcelable(default_network_info())]
            }
            "getNetworkInfoForUid" => {
                // `NetInfo`'s legacy path: `getNetworkInfoForUid(Network, uid,
                // ignoreBlocked)` answers the `NetworkInfo` of the network it
                // was given, or `null` for a network ACOS does not know.
                if args.first().is_some_and(is_default_network) {
                    vec![Value::Parcelable(default_network_info())]
                } else {
                    vec![Value::Parcelable(Parcelable::Null)]
                }
            }
            "getNetworkInfo" => {
                // `getNetworkInfo(TYPE_WIFI)` describes the emulated network;
                // every other legacy type has no network in ACOS.
                if args.first().and_then(Value::as_int) == Some(LEGACY_TYPE_WIFI) {
                    vec![Value::Parcelable(default_network_info())]
                } else {
                    vec![Value::Parcelable(Parcelable::Null)]
                }
            }
            "getNetworkForType" => {
                if args.first().and_then(Value::as_int) == Some(LEGACY_TYPE_WIFI) {
                    vec![Value::Parcelable(network_parcelable())]
                } else {
                    vec![Value::Parcelable(Parcelable::Null)]
                }
            }
            "getAllNetworks" => vec![Value::List(Some(vec![Value::Parcelable(
                network_parcelable(),
            )]))],
            "getAllNetworkInfo" => vec![Value::List(Some(vec![Value::Parcelable(
                default_network_info(),
            )]))],
            "getNetworkCapabilities" | "getNetworkCapabilitiesForPackage" => {
                // AOSP answers `null` for a null/unknown `Network`.
                if args.first().is_some_and(is_default_network) {
                    vec![Value::Parcelable(default_network_capabilities())]
                } else {
                    vec![Value::Parcelable(Parcelable::Null)]
                }
            }
            "getDefaultNetworkCapabilitiesForUser" => vec![Value::List(Some(vec![
                Value::Parcelable(default_network_capabilities()),
            ]))],
            "getActiveLinkProperties" => vec![Value::Parcelable(default_link_properties())],
            "getLinkProperties" => {
                if args.first().is_some_and(is_default_network) {
                    vec![Value::Parcelable(default_link_properties())]
                } else {
                    vec![Value::Parcelable(Parcelable::Null)]
                }
            }
            "getLinkPropertiesForType" => {
                if args.first().and_then(Value::as_int) == Some(LEGACY_TYPE_WIFI) {
                    vec![Value::Parcelable(default_link_properties())]
                } else {
                    vec![Value::Parcelable(Parcelable::Null)]
                }
            }
            "isNetworkSupported" => vec![Value::Bool(
                args.first().and_then(Value::as_int) == Some(LEGACY_TYPE_WIFI),
            )],
            "getProxyForNetwork" | "getGlobalProxy" => {
                state.proxy_lookups += 1;
                vec![Value::Parcelable(Parcelable::Null)]
            }
            "isActiveNetworkMetered" | "shouldAvoidBadWifi" | "isDefaultNetworkActive" => {
                vec![Value::Bool(false)]
            }
            "getMultipathPreference" => vec![Value::Int(0)],
            "getRestrictBackgroundStatusByCaller" => vec![Value::Int(0)],
            "getCaptivePortalServerUrl" => vec![Value::Str(Some(
                "http://connectivitycheck.gstatic.com/generate_204".to_string(),
            ))],
            "getRestoreDefaultNetworkDelay" => vec![Value::Int(0)],
            "getDefaultRequest" => {
                // AOSP's system default request: the default capabilities with
                // type `NONE`. ACOS's default request is the emulated network's
                // capabilities, id 0 (`mDefaultRequest` is created before any
                // app request).
                vec![Value::Parcelable(network_request(
                    default_network_capabilities(),
                    LEGACY_TYPE_NONE,
                    0,
                    RequestType::None,
                ))]
            }
            "reportInetCondition" | "reportNetworkConnectivity" => Vec::new(),

            // ---- callback registration --------------------------------------
            "requestNetwork" => {
                let uid = args.first().and_then(Value::as_int).unwrap_or(-1);
                let type_ordinal = args.get(2).and_then(Value::as_int).unwrap_or(-1);
                let kind = RequestType::from_ordinal(type_ordinal).ok_or_else(|| {
                    ServiceError::BadRequest {
                        interface: interface.to_string(),
                        method: method.name.clone(),
                        detail: format!("NetworkRequest.Type ordinal {type_ordinal}"),
                    }
                })?;
                // AOSP `ConnectivityService.requestNetwork`: for `TRACK_DEFAULT`
                // (and `TRACK_SYSTEM_DEFAULT`) the caller's capabilities are
                // **ignored** and replaced by the default network's
                // (`copyDefaultNetworkCapabilitiesForUid`), which is why
                // `registerDefaultNetworkCallback` — React Native's path —
                // sends `null` here. For every other type a null request is
                // `IllegalArgumentException("null NetworkCapabilities")`.
                let capabilities = match (kind, capabilities_of(args.get(1))?) {
                    (RequestType::TrackDefault | RequestType::TrackSystemDefault, _) => {
                        default_network_capabilities()
                    }
                    (_, Parcelable::Null) => {
                        return Err(ServiceError::BadRequest {
                            interface: interface.to_string(),
                            method: method.name.clone(),
                            detail: "null NetworkCapabilities for a non-TRACK_DEFAULT request"
                                .into(),
                        })
                    }
                    (_, caps) => caps,
                };
                let messenger = messenger_binder(args.get(3));
                let legacy_type = args.get(6).and_then(Value::as_int).unwrap_or(LEGACY_TYPE_NONE);
                let callback_flags = args.get(7).and_then(Value::as_int).unwrap_or(0);
                let package = args
                    .get(8)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let (request, queued) = self.register(
                    &mut state,
                    kind,
                    legacy_type,
                    callback_flags,
                    capabilities,
                    messenger,
                    uid,
                    package,
                );
                deliveries = queued;
                vec![Value::Parcelable(request)]
            }
            "listenForNetwork" => {
                let capabilities = capabilities_of(args.first())?;
                let messenger = messenger_binder(args.get(1));
                let callback_flags = args.get(3).and_then(Value::as_int).unwrap_or(0);
                let package = args
                    .get(4)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let (request, queued) = self.register(
                    &mut state,
                    RequestType::Listen,
                    LEGACY_TYPE_NONE,
                    callback_flags,
                    capabilities,
                    messenger,
                    -1,
                    package,
                );
                deliveries = queued;
                vec![Value::Parcelable(request)]
            }
            "releaseNetworkRequest" => {
                let id = request_id_of(args.first()).ok_or_else(|| ServiceError::BadRequest {
                    interface: interface.to_string(),
                    method: method.name.clone(),
                    detail: "NetworkRequest argument carries no requestId".into(),
                })?;
                if state.requests.remove(&id).is_none() {
                    // AOSP: `IllegalArgumentException("Unknown NetworkRequest")`.
                    return Err(ServiceError::BadRequest {
                        interface: interface.to_string(),
                        method: method.name.clone(),
                        detail: format!("unknown NetworkRequest id {id}"),
                    });
                }
                state.released += 1;
                Vec::new()
            }
            // The pending-intent registrations have no ACOS surface: a
            // `PendingIntent` needs a real system process to broadcast into.
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        Ok(CallResult { reply, deliveries })
    }
}

/// True when the argument is the emulated default `Network` parcelable.
fn is_default_network(value: &Value) -> bool {
    matches!(
        value,
        Value::Parcelable(Parcelable::Ordered { class, values })
            | Value::TypedObject(Parcelable::Ordered { class, values })
            if class == "android.net.Network"
        && values.first().and_then(Value::as_int) == Some(NET_ID)
    )
}

/// The `requestId` of a `NetworkRequest` argument.
fn request_id_of(value: Option<&Value>) -> Option<i32> {
    match value {
        Some(Value::Parcelable(Parcelable::Ordered { class, values }))
        | Some(Value::TypedObject(Parcelable::Ordered { class, values }))
            if class == "android.net.NetworkRequest" =>
        {
            values.get(2).and_then(Value::as_int)
        }
        _ => None,
    }
}
