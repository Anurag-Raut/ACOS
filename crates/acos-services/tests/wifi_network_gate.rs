//! `net.wifi` x `network`: no fake connectivity.
//!
//! `acos_services::network` is process-global state (one launcher process hosts
//! one session), so every test that moves it lives in this file, in **one**
//! test function, away from `wifi_module.rs`'s parallel tests.

use acos_services::module::{ModuleConfig, ModuleRegistry, Profile};
use acos_services::network;
use acos_services::ServiceHost;
use serde_json::json;

const MODULE: &str = "net.wifi";

fn start() -> (ServiceHost, ModuleRegistry) {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, MODULE, Profile::Scripted, &ModuleConfig::default())
        .expect("net.wifi attaches");
    (host, registry)
}

#[test]
fn wifi_never_contradicts_the_network_module() {
    // ---- the `network` module's `offline` control keeps the network down -----
    network::clear();
    {
        let (mut host, mut registry) = start();
        network::set_offline(true);
        registry
            .control(&mut host, MODULE, "disconnect", &json!({}))
            .expect("disconnect");
        let result = registry
            .control(&mut host, MODULE, "connect", &json!({}))
            .expect("connect");
        assert_eq!(
            result["default_network_up"], false,
            "Wi-Fi association does not resurrect a network the relay took down"
        );
        assert!(
            !host
                .connectivity_state()
                .unwrap()
                .lock()
                .unwrap()
                .default_up
        );
        network::clear();
    }

    // ---- with the relay carrying traffic, the link cannot be scripted away ----
    {
        let (mut host, mut registry) = start();
        network::set_profile(Profile::Real);
        for (op, args) in [
            ("disconnect", json!({})),
            ("enable", json!({"on": false})),
        ] {
            let error = registry
                .control(&mut host, MODULE, op, &args)
                .expect_err("refused while the network module is real");
            assert!(
                error.to_string().contains("acos_net offline"),
                "{op}: the refusal names the control that does it honestly: {error}"
            );
        }
        // Still associated, still up.
        let state = registry
            .control(&mut host, MODULE, "state", &json!({}))
            .unwrap();
        assert_eq!(state["connected"], true);
        assert_eq!(state["default_network_up"], true);
        // rssi / scan / connect stay available while networked.
        registry
            .control(&mut host, MODULE, "rssi", &json!({"rssi": -70}))
            .expect("rssi");
        registry
            .control(&mut host, MODULE, "connect", &json!({"ssid": "Other"}))
            .expect("connect");
        // And now the address facts are real: DhcpInfo/WifiInfo carry the session's.
        let info = acos_services::modules::wifi::dhcp_info_parcelable(
            &acos_services::modules::wifi::probe_state(),
        );
        let acos_services::value::Parcelable::Ordered { values, .. } = info else {
            panic!("DhcpInfo is ordered")
        };
        let ints: Vec<i32> = values
            .iter()
            .map(|v| match v {
                acos_services::value::Value::Int(i) => *i,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            ints[0],
            i32::from_le_bytes(acos_services::connectivity::NET_GUEST_ADDRESS),
            "ipAddress is the session's address when the network module is real"
        );
        assert_eq!(
            ints[1],
            i32::from_le_bytes(acos_services::connectivity::NET_GUEST_GATEWAY)
        );
        network::clear();
    }
}

/// MERGE040H: a declared feature implies its service. `android.hardware.wifi`
/// is published by `net.wifi` only (the service owner); `network` publishes no
/// Wi-Fi feature, so `network` alone never yields a null `WifiManager`.
#[test]
fn the_wifi_feature_is_published_only_by_the_module_that_serves_wifi() {
    let registry = ModuleRegistry::standard();
    let owners: Vec<&str> = registry
        .manifests()
        .into_iter()
        .filter(|m| m.features.contains(&"android.hardware.wifi"))
        .map(|m| m.id)
        .collect();
    assert_eq!(owners, vec!["net.wifi"], "only net.wifi declares FEATURE_WIFI");
}
