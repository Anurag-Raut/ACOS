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

use crate::error::{Result, ServiceError};
use crate::host::{CallResult, PendingDelivery, ServiceObject};
use crate::parcelable;
use crate::value::{Parcelable, Value};

/// The emulated default network's id (`ConnectivityService` hands these out
/// from 100 on a device).
pub const NET_ID: i32 = 100;

/// The guest's IPv4 address in the SPIKE-NET namespace (`pasta -a`).
///
/// One source of truth: `acos_art::spike_net::pasta_args` builds its `-a`/`-g`/
/// `-D` flags from these constants and this module reports them through
/// `LinkProperties`, so the interface description an app reads cannot drift from
/// the interface the namespace actually has.
pub const NET_GUEST_ADDRESS: [u8; 4] = [169, 254, 2, 15];
/// Prefix length of [`NET_GUEST_ADDRESS`] (`pasta -a 169.254.2.15/24`).
pub const NET_GUEST_PREFIX_LEN: u8 = 24;
/// pasta's gateway, which is also the DHCP server address the guest is told.
pub const NET_GUEST_GATEWAY: [u8; 4] = [169, 254, 2, 1];
/// The DNS server handed to the guest by DHCP (`pasta -D`).
pub const NET_DNS_SERVER: [u8; 4] = [1, 1, 1, 1];

/// First two octets of the per-session address block (design B2, ADR-0030
/// amendment 3).
///
/// With the shared relay every session keeps its **own** netns and its own
/// address, so `LinkProperties` is per session: [`session_network`] is the one
/// function both the transport (`acos_netd::plan`) and this module's
/// `LinkProperties` read, and the interface an app describes is therefore the
/// interface the session's netns has, by construction.
pub const NET_SESSION_BASE: [u8; 2] = [10, 163];

/// Prefix length of one session's block (`10.163.<idx>.0/30`: the relay end
/// takes `.1`, the guest `.2`).
pub const NET_SESSION_PREFIX_LEN: u8 = 30;

/// Highest session index a block can be handed out for (`10.163.254.0/30`).
pub const NET_SESSION_MAX_INDEX: u32 = 254;

/// The address, prefix and gateway of session `idx` (1-based).
///
/// `None` for `idx == 0` or `idx > NET_SESSION_MAX_INDEX`: an address block is
/// never reused silently, so the caller fails with a structured error instead.
#[must_use]
pub fn session_network(idx: u32) -> Option<SessionNetwork> {
    if idx == 0 || idx > NET_SESSION_MAX_INDEX {
        return None;
    }
    let n = u8::try_from(idx).ok()?;
    Some(SessionNetwork {
        address: [NET_SESSION_BASE[0], NET_SESSION_BASE[1], n, 2],
        prefix_len: NET_SESSION_PREFIX_LEN,
        gateway: [NET_SESSION_BASE[0], NET_SESSION_BASE[1], n, 1],
    })
}

/// One session's network, as both `LinkProperties` and the transport see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionNetwork {
    /// The guest's address in its own netns (`10.163.<idx>.2`).
    pub address: [u8; 4],
    /// Prefix length ([`NET_SESSION_PREFIX_LEN`]).
    pub prefix_len: u8,
    /// The relay end of the session's veth, the guest's default gateway.
    pub gateway: [u8; 4],
}

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
/// `ConnectivityManager.CALLBACK_LOSING` (`NetworkCallback.onLosing`).
pub const CALLBACK_LOSING: i32 = 3;
/// `ConnectivityManager.CALLBACK_LOST` (`NetworkCallback.onLost`).
///
/// **4, not 3**: `CALLBACK_LOSING` is 3 (ConnectivityManager.java:4248-4250).
/// NET6 measured the difference — sending 3 called `onLosing`, so an app
/// listening for `onLost` never saw the network go away.
pub const CALLBACK_LOST: i32 = 4;
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
    // SPIKE-NET (ADR-0030 gate): with pasta attached the session has a real,
    // NATed interface, so the network is reported as validated and apps'
    // `isInternetReachable` / ConnectivityManager checks pass. Off by default:
    // the sandbox has no sockets at all, and claiming VALIDATED there would be
    // a lie.
    let mut caps: Vec<u32> = vec![
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
    ]
    .into_iter()
    .collect();
    if spike_net_enabled() {
        caps.push(CAP_VALIDATED);
    }
    for cap in caps {
        bits |= 1i64 << cap;
    }
    bits
}

/// Does this session have a real network?
///
/// NET5: the `network` module publishes its attached profile in
/// [`crate::network`] and this is the same gate the launcher's sandbox and the
/// guest's seccomp program read, so `ConnectivityManager`, the netns and
/// `AF_INET` cannot disagree. With the module off the answer is the honest
/// no-network device ACOS has always presented (no address, no DNS, no route,
/// no `VALIDATED`); the `ACOS_SPIKE_NET` override only remains for the standalone
/// `acos-art` binary's own spikes.
fn spike_net_enabled() -> bool {
    crate::network::session_has_network()
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
    /// Is there a default network at all? `false` while the `network` module's
    /// `offline` control is in effect (NET6): `getActiveNetwork()` answers
    /// `null`, every registered callback got `onLost`, and a new registration
    /// gets no `onAvailable` until `online` restores it — exactly the state
    /// Android presents when the only network is lost.
    pub default_up: bool,
    /// Recorded proxy lookups (`Network` presence marker).
    pub proxy_lookups: u32,
    /// Live requests, in registration order.
    pub requests: BTreeMap<i32, RequestRecord>,
    /// Callback messages sent (evidence counter).
    pub callbacks_sent: u64,
    /// Requests released (evidence counter).
    pub released: u64,
    /// Live requests' `IMessenger` callbacks as host-held strong references
    /// (`crate::refs`): `registerNetworkCallback`/`listenForNetwork` take one,
    /// `releaseNetworkRequest`/`unregisterNetworkCallback` drop it.
    pub refs: crate::refs::GuestRefs,
    next_request_id: i32,
}

impl ConnectivityState {
    /// New empty state, with the session's default network up (the module
    /// publishes `offline` only when its control is used).
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_request_id: 1,
            default_up: true,
            ..Self::default()
        }
    }

    /// Move the default network between up and down, returning the callback
    /// messages the change produces for every live request.
    ///
    /// * up → down: one `CALLBACK_LOST` per request (AOSP's
    ///   `ConnectivityService` sends only `Network` with `onLost`);
    /// * down → up: the four registration messages again
    ///   ([`callback_messages`]), i.e. `onAvailable`, `onCapabilitiesChanged`,
    ///   `onLinkPropertiesChanged`, `onBlockedStatusChanged` — what Android
    ///   delivers when a network reappears.
    ///
    /// A no-op when the state does not change: a doubled `offline` must not fire
    /// a second `onLost` (Android does not either).
    pub fn set_default_network(&mut self, up: bool) -> Vec<PendingDelivery> {
        if self.default_up == up {
            return Vec::new();
        }
        self.default_up = up;
        let mut out = Vec::new();
        for record in self.requests.values() {
            let Some(target) = record.messenger else {
                continue;
            };
            if up {
                for (code, _arg1, data) in callback_messages(&record.request) {
                    out.push(PendingDelivery {
                        target,
                        interface: "android.os.IMessenger".to_string(),
                        code: IMESSENGER_SEND,
                        method: callback_method(code).to_string(),
                        data,
                    });
                }
            } else {
                out.push(PendingDelivery {
                    target,
                    interface: "android.os.IMessenger".to_string(),
                    code: IMESSENGER_SEND,
                    method: "CALLBACK_LOST".to_string(),
                    data: encode_message(
                        CALLBACK_LOST,
                        0,
                        &[
                            ("NetworkRequest", &record.request),
                            ("Network", &network_parcelable()),
                        ],
                    )
                    .expect("callback bundle"),
                });
            }
        }
        self.callbacks_sent += out.len() as u64;
        out
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
///
/// The value list is `NetworkCapabilities.writeToParcel`'s field order, one
/// value per [`crate::parcelable`] field op: the two nullable parcelables are
/// `writeParcelable(null)` (`NamedParcelable(Null)`), `mUids` is a `null`
/// `ArraySet` (`writeInt(-1)`, no elements), the underlying networks are a
/// `null` `writeTypedList` (`-1`). The byte stream is unchanged from the RET1
/// reply; only the ops that describe it changed, so a future non-null
/// specifier, uid set or underlying network encodes in the framework's order.
#[must_use]
pub fn default_network_capabilities() -> Parcelable {
    Parcelable::ordered(
        "android.net.NetworkCapabilities",
        vec![
            Value::Long(default_capabilities_bits()),
            Value::Long(0),                           // forbidden
            Value::Long(1i64 << TRANSPORT_WIFI),      // transports
            Value::Int(0),                            // link up kbps
            Value::Int(0),                            // link down kbps
            Value::NamedParcelable(Parcelable::Null), // NetworkSpecifier
            Value::NamedParcelable(Parcelable::Null), // TransportInfo
            Value::Int(i32::MIN),                     // SIGNAL_STRENGTH_UNSPECIFIED
            Value::List(None),                        // mUids (null ArraySet)
            Value::IntArray(Some(Vec::new())),        // mAllowedUids
            Value::Str(None),                         // mSSID
            Value::Bool(false),                       // mPrivateDnsBroken
            Value::IntArray(Some(Vec::new())),        // administrator uids
            Value::Int(1000),                         // owner uid (system)
            Value::Int(1000),                         // requestor uid
            Value::Str(Some("android".to_string())),  // requestor package
            Value::IntArray(Some(Vec::new())),        // mSubIds
            Value::List(None),                        // underlying networks (null)
            Value::Int(0),                            // enterprise ids
        ],
    )
}

/// `android.net.LinkProperties` of the emulated default network.
///
/// Without the network module there is no interface in the sandbox, so the three
/// address lists, the route list and the stacked-link list are empty and the
/// DHCP server address is null. The interface name and MTU are the AOSP
/// emulator's wifi defaults, so `getInterfaceName()` and `getMtu()` answer with a
/// plausible, documented value instead of `null`.
///
/// SPIKE-NET (ADR-0030 gate, item 5): with pasta attached the session really has
/// an interface, an address, a DNS server and a default route, so
/// `getLinkProperties()` describes them instead of lying by omission — the guest
/// address pasta hands out (`169.254.2.15/24`), its gateway
/// (`169.254.2.1`, which is also the DHCP server) and the DHCP-answered DNS
/// server (`1.1.1.1`, `spike_net::pasta_args`' `-D`). Nothing is invented: the
/// values are the ones the namespace actually has.
///
/// The value list follows the framework's writer field for field: the list
/// counts are [`Value::List`]s consumed by [`crate::parcelable::FieldOp::Counted`]
/// (or [`Value::List`] for `writeList`), the HTTP proxy is an absent
/// [`Value::Cond`] (a `writeByte(0)` marker), and every `writeParcelable(null)`
/// is a [`Value::NamedParcelable`] holding [`Parcelable::Null`].
#[must_use]
pub fn default_link_properties() -> Parcelable {
    link_properties(spike_net_enabled())
}

/// [`default_link_properties`] with the network gate passed in (testable: the
/// gate is process-global).
///
/// REL3P2's oracle replaced the old flat `writeInt` count fields with
/// [`FieldOp::Counted`] element lists, so a non-empty list's elements are
/// `Value::Cond { present: true, values }` — one value per op of the collapsed
/// loop body (`writeParcelable(LinkAddress)` → [`FieldOp::Named`] → a
/// `NamedParcelable`; `writeAddress(InetAddress)` → `writeByteArray` +
/// `writeBoolean(hasScopeId)` + `writeInt(scopeId)`). The old bare
/// `Value::Parcelable`/`Value::ByteArray` elements made the writer reject its
/// own value (`expected counted element`) and panicked the launcher the first
/// time a real session asked for a network callback (NETREAL). The empty arms
/// below are unchanged: a zero count is the same `writeInt(0)` either way.
#[must_use]
pub fn link_properties(networked: bool) -> Parcelable {
    let counted = |values: Vec<Value>| Value::Cond {
        present: true,
        values,
    };
    let (addrs, dns, dhcp, routes) = if networked {
        (
            Value::List(Some(vec![counted(vec![Value::NamedParcelable(
                guest_link_address(),
            )])])),
            Value::List(Some(vec![counted(vec![
                Value::ByteArray(Some(dns_server_bytes())),
                Value::Bool(false), // hasScopeId (an IPv4 address)
                Value::Int(0),      // scopeId
            ])])),
            Value::ByteArray(Some(dhcp_server_bytes())),
            Value::List(Some(vec![counted(vec![Value::NamedParcelable(
                default_route(),
            )])])),
        )
    } else {
        (
            Value::List(Some(Vec::new())),
            Value::List(Some(Vec::new())),
            Value::ByteArray(None),
            Value::List(Some(Vec::new())),
        )
    };
    Parcelable::ordered(
        "android.net.LinkProperties",
        vec![
            Value::Str(Some("wlan0".to_string())),
            addrs,                         // link addresses
            dns,                           // DNS servers
            Value::List(Some(Vec::new())), // validated private DNS servers
            Value::Bool(false),            // usePrivateDns
            Value::Str(None),              // privateDnsServerName
            Value::List(Some(Vec::new())), // PCSCF servers
            Value::Str(None),              // domains
            dhcp,                          // dhcp server address
            Value::Int(1500),              // mtu
            Value::Str(None),              // tcpBufferSizes
            routes,                        // routes
            Value::Cond {
                present: false,
                values: Vec::new(),
            }, // httpProxy
            Value::NamedParcelable(Parcelable::Null), // nat64Prefix
            Value::List(Some(Vec::new())), // stacked links (`writeList`)
            Value::Bool(false),            // wakeOnLanSupported
            Value::NamedParcelable(Parcelable::Null), // captivePortalApiUrl
            Value::NamedParcelable(Parcelable::Null), // captivePortalData
        ],
    )
}

/// The guest's address in the SPIKE-NET namespace (`NET_GUEST_ADDRESS/24`),
/// as a `LinkAddress`: scope link (`RT_SCOPE_LINK` = 253), no flags, unknown
/// lifetimes (the namespace has no address lifetime).
fn guest_link_address() -> Parcelable {
    Parcelable::ordered(
        "android.net.LinkAddress",
        vec![
            Value::ByteArray(Some(NET_GUEST_ADDRESS.to_vec())),
            Value::Int(i32::from(NET_GUEST_PREFIX_LEN)),
            Value::Int(0),   // flags
            Value::Int(253), // RT_SCOPE_LINK
            Value::Long(-1), // deprecationTime = LIFETIME_UNKNOWN
            Value::Long(-1), // expirationTime = LIFETIME_UNKNOWN
        ],
    )
}

/// The default route of the namespace: `0.0.0.0/0` via pasta's gateway
/// (`NET_GUEST_GATEWAY`), `RTN_UNICAST`, no MTU override.
fn default_route() -> Parcelable {
    Parcelable::ordered(
        "android.net.RouteInfo",
        vec![
            Value::NamedParcelable(Parcelable::ordered(
                "android.net.IpPrefix",
                vec![
                    Value::ByteArray(Some(vec![0, 0, 0, 0])),
                    Value::Int(0), // /0
                ],
            )),
            Value::ByteArray(Some(NET_GUEST_GATEWAY.to_vec())),
            Value::Str(Some("wlan0".to_string())),
            Value::Int(1), // RTN_UNICAST
            Value::Int(0), // no per-route MTU
        ],
    )
}

fn dns_server_bytes() -> Vec<u8> {
    NET_DNS_SERVER.to_vec()
}

fn dhcp_server_bytes() -> Vec<u8> {
    NET_GUEST_GATEWAY.to_vec()
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
/// here), `when`, the data bundle, the `replyTo` messenger (a null strong binder
/// is a single `0` word, not a flat object) and `sendingUid`.
///
/// **The image has one uid word, not two.** `android-14.0.0_r75`'s
/// `Message.writeToParcel` ends at `sendingUid`; the r75 source cache also shows
/// `workSourceUid`. NET6 measured the difference end to end: an extra word made
/// the app's `IMessenger.Stub.onTransact` answer
/// `Parcel data not fully consumed, unread size: 4`, so `NetworkCallback.onLost`
/// and `onAvailable` never fired. The image is authoritative (per the common
/// brief), and `net1_connectivity` pins the byte count.
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

/// The `method` label a callback delivery carries (evidence, not wire).
fn callback_method(code: i32) -> &'static str {
    match code {
        CALLBACK_AVAILABLE => "CALLBACK_AVAILABLE",
        CALLBACK_CAP_CHANGED => "CALLBACK_CAP_CHANGED",
        CALLBACK_IP_CHANGED => "CALLBACK_IP_CHANGED",
        CALLBACK_LOST => "CALLBACK_LOST",
        _ => "CALLBACK_BLK_CHANGED",
    }
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
        // NET6: a registration while the session is offline is parked, not
        // answered: Android delivers nothing to a default-network callback until
        // a network exists (the app sees `onLost` if it registered earlier, and
        // nothing at all if it registers later).
        if let Some(target) = messenger.filter(|_| state.default_up) {
            // AOSP `ConnectivityService.callCallbackForRequest` returns early
            // when the request has no messenger (the system default request),
            // and ACOS follows it: no messenger, no callback messages.
            for (code, _arg1, data) in callback_messages(&reply) {
                deliveries.push(PendingDelivery {
                    target,
                    interface: "android.os.IMessenger".to_string(),
                    code: IMESSENGER_SEND,
                    method: callback_method(code).to_string(),
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
    /// Registered network callbacks are host-held strong references
    /// (`registerNetworkCallback`/`releaseNetworkRequest`), drained through the
    /// shared `ConnectivityState::refs` ledger.
    fn drain_retained(&mut self) -> Vec<BinderId> {
        self.state.lock().unwrap().refs.drain_retained()
    }

    fn drain_released(&mut self) -> Vec<BinderId> {
        self.state.lock().unwrap().refs.drain_released()
    }

    fn release_all_refs(&mut self) -> Vec<BinderId> {
        self.state.lock().unwrap().refs.release_all()
    }

    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.net.IConnectivityManager" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let mut state = self.state.lock().unwrap();
        // NET6: while the `network` module's `offline` control is in effect there
        // is no default network, and every question about it answers the way
        // Android answers with none: `null`, an empty list, or `false`.
        let up = state.default_up;
        let mut deliveries = Vec::new();
        let reply = match method.name.as_str() {
            // ---- network queries -------------------------------------------
            "getActiveNetwork" | "getActiveNetworkForUid" => vec![Value::Parcelable(if up {
                network_parcelable()
            } else {
                Parcelable::Null
            })],
            "getActiveNetworkInfo" | "getActiveNetworkInfoForUid" => {
                vec![Value::Parcelable(if up {
                    default_network_info()
                } else {
                    Parcelable::Null
                })]
            }
            "getNetworkInfoForUid" => {
                // `NetInfo`'s legacy path: `getNetworkInfoForUid(Network, uid,
                // ignoreBlocked)` answers the `NetworkInfo` of the network it
                // was given, or `null` for a network ACOS does not know.
                if up && args.first().is_some_and(is_default_network) {
                    vec![Value::Parcelable(default_network_info())]
                } else {
                    vec![Value::Parcelable(Parcelable::Null)]
                }
            }
            "getNetworkInfo" => {
                // `getNetworkInfo(TYPE_WIFI)` describes the emulated network;
                // every other legacy type has no network in ACOS.
                if up && args.first().and_then(Value::as_int) == Some(LEGACY_TYPE_WIFI) {
                    vec![Value::Parcelable(default_network_info())]
                } else {
                    vec![Value::Parcelable(Parcelable::Null)]
                }
            }
            "getNetworkForType" => {
                if up && args.first().and_then(Value::as_int) == Some(LEGACY_TYPE_WIFI) {
                    vec![Value::Parcelable(network_parcelable())]
                } else {
                    vec![Value::Parcelable(Parcelable::Null)]
                }
            }
            "getAllNetworks" => vec![Value::List(Some(if up {
                vec![Value::Parcelable(network_parcelable())]
            } else {
                Vec::new()
            }))],
            "getAllNetworkInfo" => vec![Value::List(Some(if up {
                vec![Value::Parcelable(default_network_info())]
            } else {
                Vec::new()
            }))],
            "getNetworkCapabilities" | "getNetworkCapabilitiesForPackage" => {
                // AOSP answers `null` for a null/unknown `Network`.
                if up && args.first().is_some_and(is_default_network) {
                    vec![Value::Parcelable(default_network_capabilities())]
                } else {
                    vec![Value::Parcelable(Parcelable::Null)]
                }
            }
            "getDefaultNetworkCapabilitiesForUser" => vec![Value::List(Some(if up {
                vec![Value::Parcelable(default_network_capabilities())]
            } else {
                Vec::new()
            }))],
            "getActiveLinkProperties" => vec![Value::Parcelable(if up {
                default_link_properties()
            } else {
                Parcelable::Null
            })],
            "getLinkProperties" => {
                if up && args.first().is_some_and(is_default_network) {
                    vec![Value::Parcelable(default_link_properties())]
                } else {
                    vec![Value::Parcelable(Parcelable::Null)]
                }
            }
            "getLinkPropertiesForType" => {
                if up && args.first().and_then(Value::as_int) == Some(LEGACY_TYPE_WIFI) {
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
            "isActiveNetworkMetered" | "shouldAvoidBadWifi" => vec![Value::Bool(false)],
            "isDefaultNetworkActive" => vec![Value::Bool(up)],
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
                let legacy_type = args
                    .get(6)
                    .and_then(Value::as_int)
                    .unwrap_or(LEGACY_TYPE_NONE);
                let callback_flags = args.get(7).and_then(Value::as_int).unwrap_or(0);
                let package = args
                    .get(8)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if let Some(messenger) = messenger {
                    // A registered callback is a strong reference on the guest's
                    // messenger (a repeat registration of the same binder is not
                    // a second one).
                    state.refs.hold(messenger);
                }
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
                if let Some(messenger) = messenger {
                    state.refs.hold(messenger);
                }
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
                let removed = state.requests.remove(&id);
                if removed.is_none() {
                    // AOSP: `IllegalArgumentException("Unknown NetworkRequest")`.
                    return Err(ServiceError::BadRequest {
                        interface: interface.to_string(),
                        method: method.name.clone(),
                        detail: format!("unknown NetworkRequest id {id}"),
                    });
                }
                if let Some(messenger) = removed.and_then(|record| record.messenger) {
                    state.refs.release(messenger);
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

#[cfg(test)]
mod link_properties_tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// The flag is process-wide, so these tests take turns.
    static ENV: Mutex<()> = Mutex::new(());

    fn with_flag<T>(on: bool, f: impl FnOnce() -> T) -> T {
        let _guard: MutexGuard<'_, ()> = ENV.lock().unwrap_or_else(|e| e.into_inner());
        // The mutex makes this the only thread of this test binary that touches
        // the flag; no other test reads it (`grep ACOS_SPIKE_NET`).
        if on {
            std::env::set_var("ACOS_SPIKE_NET", "1");
        } else {
            std::env::remove_var("ACOS_SPIKE_NET");
        }
        let out = f();
        std::env::remove_var("ACOS_SPIKE_NET");
        out
    }

    fn list(value: &Value) -> Vec<Value> {
        match value {
            Value::List(Some(items)) => items.clone(),
            other => panic!("expected a list, got {other:?}"),
        }
    }

    /// One element of a `FieldOp::Counted` list, in the shape the writer accepts
    /// and the reader produces (`Value::Cond { present: true, values }`).
    fn counted(value: &Value) -> Vec<Value> {
        match value {
            Value::Cond {
                present: true,
                values,
            } => values.clone(),
            other => panic!("expected a counted element, got {other:?}"),
        }
    }

    /// The payload the relay actually sends (a `NetworkCallback`'s
    /// `LinkProperties`) must survive `write_payload` and read back as the value
    /// it was built from.
    ///
    /// This is the test the real-network arms needed and did not have: the old
    /// value put a bare `LinkAddress` parcelable where `LinkProperties`' spec
    /// expects a counted element, so the *writer* rejected its own value and the
    /// launcher panicked at the first callback on a networked session (NETREAL).
    /// A shape assertion alone could not see that; writing the bytes can.
    #[test]
    fn the_wire_payload_round_trips_on_both_arms() {
        for networked in [false, true] {
            let value = link_properties(networked);
            let mut parcel = Parcel::new();
            crate::parcelable::write_payload(&mut parcel, &value)
                .unwrap_or_else(|e| panic!("networked={networked}: write: {e}"));
            parcel.set_data_position(0);
            let back = crate::parcelable::read_payload("android.net.LinkProperties", &mut parcel)
                .unwrap_or_else(|e| panic!("networked={networked}: read: {e}"));
            assert_eq!(back, value, "networked={networked}: byte round trip");
        }
    }

    /// A `NetworkCallback`'s whole `Message` bundle is written the same way: the
    /// four registration messages are exactly what a networked session sends.
    #[test]
    fn callback_messages_encode_on_both_arms() {
        let request = Parcelable::ordered(
            "android.net.NetworkRequest",
            vec![
                Value::NamedParcelable(default_network_capabilities()),
                Value::Int(0),
                Value::Int(1),
                Value::Str(Some("default".to_string())),
            ],
        );
        for networked in [false, true] {
            let with_flag_value = with_flag(networked, || callback_messages(&request));
            assert_eq!(with_flag_value.len(), 4, "networked={networked}");
            for (code, _, parcel) in with_flag_value {
                assert!(
                    parcel.data_size() > 0,
                    "networked={networked}: message {code} wrote bytes"
                );
            }
        }
    }

    /// Item 5 parity: with the network attached, `getLinkProperties()` describes
    /// the interface the namespace really has — the guest address, the DNS
    /// server DHCP handed out, and a default route through pasta's gateway.
    #[test]
    fn flag_on_reports_the_namespaces_interface_addresses_dns_and_route() {
        let lp = with_flag(true, default_link_properties);
        let fields = lp.fields().expect("an ordered LinkProperties payload");

        assert_eq!(
            fields[0],
            Value::Str(Some("wlan0".to_string())),
            "interface name"
        );

        let addrs = list(&fields[1]);
        assert_eq!(addrs.len(), 1, "one link address");
        let addr = counted(&addrs[0]);
        let Value::NamedParcelable(Parcelable::Ordered { class, values }) = &addr[0] else {
            panic!("a named LinkAddress parcelable");
        };
        assert_eq!(class, "android.net.LinkAddress");
        assert_eq!(
            values[0],
            Value::ByteArray(Some(NET_GUEST_ADDRESS.to_vec())),
            "the address pasta configures"
        );
        assert_eq!(values[1], Value::Int(i32::from(NET_GUEST_PREFIX_LEN)));

        let dns = list(&fields[2]);
        assert_eq!(dns.len(), 1, "one DNS server");
        let dns_element = counted(&dns[0]);
        assert_eq!(
            dns_element[0],
            Value::ByteArray(Some(NET_DNS_SERVER.to_vec())),
            "the resolver DHCP handed the namespace"
        );
        assert_eq!(dns_element[1], Value::Bool(false), "an IPv4 address has no scope id");
        assert_eq!(dns_element[2], Value::Int(0));

        assert_eq!(fields[3], Value::List(Some(Vec::new())), "no private DNS");
        assert_eq!(
            fields[8],
            Value::ByteArray(Some(NET_GUEST_GATEWAY.to_vec())),
            "the DHCP server is pasta's gateway"
        );

        let routes = list(&fields[11]);
        assert_eq!(routes.len(), 1, "one default route");
        let route = counted(&routes[0]);
        let Value::NamedParcelable(Parcelable::Ordered { class, values }) = &route[0] else {
            panic!("a named RouteInfo parcelable");
        };
        assert_eq!(class, "android.net.RouteInfo");
        assert_eq!(
            values[1],
            Value::ByteArray(Some(NET_GUEST_GATEWAY.to_vec())),
            "the route's gateway"
        );
        assert_eq!(values[2], Value::Str(Some("wlan0".to_string())));
    }

    /// The flag-off payload is the one ACOS shipped: nothing to describe, and the
    /// wire bytes are an explicit zero count per list (the previous `Int(0)`
    /// fields and an empty `List` are byte-identical).
    #[test]
    fn flag_off_keeps_every_list_empty() {
        let lp = with_flag(false, default_link_properties);
        let fields = lp.fields().expect("an ordered LinkProperties payload");
        for index in [1, 2, 3, 6, 11] {
            assert_eq!(
                fields[index],
                Value::List(Some(Vec::new())),
                "field {index} must stay empty without the network module"
            );
        }
        assert_eq!(fields[8], Value::ByteArray(None), "no DHCP server address");
    }

    /// The flag adds `NET_CAPABILITY_VALIDATED` and nothing else.
    #[test]
    fn validated_capability_follows_the_flag() {
        let bit = 1i64 << CAP_VALIDATED;
        let off = with_flag(false, default_capabilities_bits);
        assert_eq!(off & bit, 0, "flag off must not claim a validated network");
        let on = with_flag(true, default_capabilities_bits);
        assert_ne!(on & bit, 0, "flag on reports the network as validated");
        assert_eq!(on & !bit, off, "no other capability changes");
    }
}
