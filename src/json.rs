use std::collections::HashMap;

use itertools::Itertools;
use log::debug;

use crate::networkd;
use crate::MonitordStats;

fn flatten_networkd(networkd_stats: &networkd::NetworkdState) -> HashMap<String, u64> {
    let mut flat_stats: HashMap<String, u64> = HashMap::new();
    let base_metric_name = "networkd";

    let managed_interfaces_key = format!("{}.managed_interfaces", base_metric_name);
    flat_stats.insert(managed_interfaces_key, networkd_stats.managed_interfaces);

    if networkd_stats.interfaces_state.is_empty() {
        debug!("No netword interfaces to add to flat JSON");
        return flat_stats;
    }

    for interface in &networkd_stats.interfaces_state {
        println!("{:?}", interface);
        let interface_base = format!("{}.{}", base_metric_name, interface.name);
        flat_stats.insert(
            format!("{interface_base}.address_state"),
            interface.address_state as u64,
        );
        flat_stats.insert(
            format!("{interface_base}.admin_state"),
            interface.admin_state as u64,
        );
        flat_stats.insert(
            format!("{interface_base}.carrier_state"),
            interface.carrier_state as u64,
        );
        flat_stats.insert(
            format!("{interface_base}.ipv4_address_state"),
            interface.ipv4_address_state as u64,
        );
        flat_stats.insert(
            format!("{interface_base}.ipv6_address_state"),
            interface.ipv6_address_state as u64,
        );
        flat_stats.insert(
            format!("{interface_base}.oper_state"),
            interface.oper_state as u64,
        );
        flat_stats.insert(
            format!("{interface_base}.required_for_online"),
            interface.required_for_online as u64,
        );
    }
    flat_stats
}

/// Take the standard returned structs and move all to a flat HashMap<str, float|int> like JSON
pub fn flatten(stats_struct: &MonitordStats) -> String {
    let mut flat_stats: HashMap<String, u64> = HashMap::new();

    // Add networkd stats
    flat_stats.extend(flatten_networkd(&stats_struct.networkd));

    let mut json_str = String::from("{\n");
    for (key, value) in flat_stats.iter().sorted() {
        let new_kv = format!("  '{}': {},\n", key, value);
        json_str.push_str(new_kv.as_str());
    }
    // Remove last trailing comma to be valid JSON - Super lame but works ...
    json_str.pop();
    json_str.pop();
    json_str.push_str("\n}");
    json_str
}

#[cfg(test)]
mod tests {
    use super::*;

    // This will always be sorted / deterministic ...
    const EXPECTED_FLAT_JSON: &str = r###"{
  'networkd.eth0.address_state': 3,
  'networkd.eth0.admin_state': 4,
  'networkd.eth0.carrier_state': 5,
  'networkd.eth0.ipv4_address_state': 3,
  'networkd.eth0.ipv6_address_state': 2,
  'networkd.eth0.oper_state': 9,
  'networkd.eth0.required_for_online': 1,
  'networkd.managed_interfaces': 1
}"###;

    fn return_monitord_stats() -> MonitordStats {
        MonitordStats {
            networkd: networkd::NetworkdState {
                interfaces_state: vec![networkd::InterfaceState {
                    address_state: networkd::AddressState::routable,
                    admin_state: networkd::AdminState::configured,
                    carrier_state: networkd::CarrierState::carrier,
                    ipv4_address_state: networkd::AddressState::routable,
                    ipv6_address_state: networkd::AddressState::degraded,
                    name: "eth0".to_string(),
                    network_file: "/etc/systemd/network/69-eno4.network".to_string(),
                    oper_state: networkd::OperState::routable,
                    required_for_online: networkd::BoolState::True,
                }],
                managed_interfaces: 1,
            },
        }
    }

    #[test]
    fn test_flatten() {
        assert_eq!(EXPECTED_FLAT_JSON, flatten(&return_monitord_stats()));
    }
}
