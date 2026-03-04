//! # varlink_networkd module
//!
//! Collection of systemd-networkd statistics via the `io.systemd.Network` varlink API.
//! Available from systemd v257+.
//!
//! When enabled via `enable_varlink = true` in the `[networkd]` config section, monitord
//! will attempt to collect interface states via varlink and fall back to parsing
//! `/run/systemd/netif/links` state files on failure.

use std::str::FromStr;

use tracing::debug;

use crate::networkd::{
    AddressState, AdminState, BoolState, CarrierState, InterfaceState, NetworkdState, OperState,
};
use crate::varlink::network::{Interface, Network};

pub use crate::varlink::network::NETWORK_SOCKET_PATH;

/// Map a varlink [`Interface`] to our [`InterfaceState`] struct.
fn map_interface(iface: &Interface) -> InterfaceState {
    let address_state = AddressState::from_str(&iface.address_state).unwrap_or_else(|_| {
        debug!("Unknown address_state value: {:?}", iface.address_state);
        AddressState::unknown
    });
    let admin_state = AdminState::from_str(&iface.administrative_state).unwrap_or_else(|_| {
        debug!(
            "Unknown administrative_state value: {:?}",
            iface.administrative_state
        );
        AdminState::unknown
    });
    let carrier_state = CarrierState::from_str(&iface.carrier_state).unwrap_or_else(|_| {
        debug!("Unknown carrier_state value: {:?}", iface.carrier_state);
        CarrierState::unknown
    });
    let ipv4_address_state =
        AddressState::from_str(&iface.ipv4_address_state).unwrap_or_else(|_| {
            debug!(
                "Unknown ipv4_address_state value: {:?}",
                iface.ipv4_address_state
            );
            AddressState::unknown
        });
    let ipv6_address_state =
        AddressState::from_str(&iface.ipv6_address_state).unwrap_or_else(|_| {
            debug!(
                "Unknown ipv6_address_state value: {:?}",
                iface.ipv6_address_state
            );
            AddressState::unknown
        });
    let oper_state = OperState::from_str(&iface.operational_state).unwrap_or_else(|_| {
        debug!(
            "Unknown operational_state value: {:?}",
            iface.operational_state
        );
        OperState::unknown
    });
    let required_for_online = match iface.required_for_online {
        Some(true) => BoolState::True,
        Some(false) => BoolState::False,
        None => BoolState::unknown,
    };
    let network_file = iface.network_file.clone().unwrap_or_default();

    InterfaceState {
        address_state,
        admin_state,
        carrier_state,
        ipv4_address_state,
        ipv6_address_state,
        name: iface.name.clone(),
        network_file,
        oper_state,
        required_for_online,
    }
}

/// Collect networkd interface stats via the `io.systemd.Network` varlink `Describe` method.
///
/// Runs on a blocking thread with a dedicated runtime because the zlink connection
/// is `!Send` and cannot be held across `await` points in a `Send` future.
pub async fn get_networkd_state(socket_path: &str) -> anyhow::Result<NetworkdState> {
    let socket_path = socket_path.to_string();
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async move {
            let mut conn = zlink::unix::connect(&socket_path).await?;
            let result = conn.describe().await?;
            match result {
                Ok(output) => {
                    let mut interfaces_state = Vec::new();
                    let mut managed_interfaces: u64 = 0;
                    for iface in output.interfaces.unwrap_or_default() {
                        // Only count interfaces that have a network configuration file –
                        // the same criterion used by the file-based parser.
                        if iface.network_file.is_none() {
                            continue;
                        }
                        managed_interfaces += 1;
                        interfaces_state.push(map_interface(&iface));
                    }
                    Ok(NetworkdState {
                        interfaces_state,
                        managed_interfaces,
                    })
                }
                Err(e) => Err(anyhow::anyhow!("io.systemd.Network.Describe error: {}", e)),
            }
        })
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_interface_full() {
        let iface = Interface {
            index: 2,
            name: "eth0".to_string(),
            administrative_state: "configured".to_string(),
            operational_state: "routable".to_string(),
            carrier_state: "carrier".to_string(),
            address_state: "routable".to_string(),
            ipv4_address_state: "degraded".to_string(),
            ipv6_address_state: "routable".to_string(),
            online_state: Some("online".to_string()),
            network_file: Some("/etc/systemd/network/69-eno4.network".to_string()),
            required_for_online: Some(true),
        };
        let state = map_interface(&iface);
        assert_eq!(state.name, "eth0");
        assert_eq!(state.admin_state, AdminState::configured);
        assert_eq!(state.oper_state, OperState::routable);
        assert_eq!(state.carrier_state, CarrierState::carrier);
        assert_eq!(state.address_state, AddressState::routable);
        assert_eq!(state.ipv4_address_state, AddressState::degraded);
        assert_eq!(state.ipv6_address_state, AddressState::routable);
        assert_eq!(state.required_for_online, BoolState::True);
        assert_eq!(
            state.network_file,
            "/etc/systemd/network/69-eno4.network".to_string()
        );
    }

    #[test]
    fn test_map_interface_unknown_states() {
        let iface = Interface {
            index: 3,
            name: "wg0".to_string(),
            administrative_state: "initialized".to_string(), // new state in systemd 257
            operational_state: "unknown-oper".to_string(),
            carrier_state: "no-carrier".to_string(),
            address_state: "off".to_string(),
            ipv4_address_state: "off".to_string(),
            ipv6_address_state: "off".to_string(),
            online_state: None,
            network_file: Some("/etc/systemd/network/wg0.network".to_string()),
            required_for_online: Some(false),
        };
        let state = map_interface(&iface);
        assert_eq!(state.admin_state, AdminState::unknown);
        assert_eq!(state.oper_state, OperState::unknown);
        assert_eq!(state.carrier_state, CarrierState::no_carrier);
        assert_eq!(state.address_state, AddressState::off);
        assert_eq!(state.required_for_online, BoolState::False);
    }

    #[test]
    fn test_map_interface_no_required_for_online() {
        let iface = Interface {
            index: 1,
            name: "lo".to_string(),
            administrative_state: "unmanaged".to_string(),
            operational_state: "carrier".to_string(),
            carrier_state: "carrier".to_string(),
            address_state: "degraded".to_string(),
            ipv4_address_state: "degraded".to_string(),
            ipv6_address_state: "degraded".to_string(),
            online_state: None,
            network_file: None,
            required_for_online: None,
        };
        let state = map_interface(&iface);
        assert_eq!(state.required_for_online, BoolState::unknown);
        assert_eq!(state.network_file, "");
    }
}
