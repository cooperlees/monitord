use std::collections::HashMap;

use itertools::Itertools;
use log::debug;

use crate::networkd;
use crate::units;
use crate::MonitordStats;

fn flatten_networkd(networkd_stats: &networkd::NetworkdState) -> HashMap<String, u64> {
    let mut flat_stats: HashMap<String, u64> = HashMap::new();
    let base_metric_name = "networkd";

    let managed_interfaces_key = format!("{}.managed_interfaces", base_metric_name);
    flat_stats.insert(managed_interfaces_key, networkd_stats.managed_interfaces);

    if networkd_stats.interfaces_state.is_empty() {
        debug!("No networkd interfaces to add to flat JSON");
        return flat_stats;
    }

    for interface in &networkd_stats.interfaces_state {
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

fn flatten_units(units_stats: &units::SystemdUnitStats) -> HashMap<String, u64> {
    let mut flat_stats: HashMap<String, u64> = HashMap::new();
    let base_metric_name = "units";

    // TODO: Work out a smarter way to do this rather than hard code mappings
    for field_name in units::UNIT_FIELD_NAMES {
        let key = format!("{base_metric_name}.{field_name}");
        let value: Option<u64> = match field_name.to_string().as_str() {
            "active_units" => Some(units_stats.active_units),
            "automount_units" => Some(units_stats.automount_units),
            "device_units" => Some(units_stats.device_units),
            "failed_units" => Some(units_stats.failed_units),
            "inactive_units" => Some(units_stats.inactive_units),
            "jobs_queued" => Some(units_stats.jobs_queued),
            "loaded_units" => Some(units_stats.loaded_units),
            "masked_units" => Some(units_stats.masked_units),
            "mount_units" => Some(units_stats.mount_units),
            "not_found_units" => Some(units_stats.not_found_units),
            "path_units" => Some(units_stats.path_units),
            "scope_units" => Some(units_stats.scope_units),
            "service_units" => Some(units_stats.service_units),
            "slice_units" => Some(units_stats.slice_units),
            "socket_units" => Some(units_stats.socket_units),
            "target_units" => Some(units_stats.target_units),
            "timer_units" => Some(units_stats.timer_units),
            "total_units" => Some(units_stats.total_units),
            _ => {
                debug!("Got a unhandled stat '{}'", field_name);
                None
            }
        };
        if let Some(an_integer) = value {
            flat_stats.insert(key, an_integer);
        }
    }
    flat_stats
}

/// Take the standard returned structs and move all to a flat HashMap<str, float|int> like JSON
pub fn flatten_hashmap(stats_struct: &MonitordStats) -> HashMap<String, u64> {
    let mut flat_stats: HashMap<String, u64> = HashMap::new();
    flat_stats.extend(flatten_networkd(&stats_struct.networkd));
    flat_stats.extend(flatten_units(&stats_struct.units));
    flat_stats
}

/// Take the standard returned structs and move all to a flat JSON str
pub fn flatten(stats_struct: &MonitordStats) -> String {
    let flat_stats = flatten_hashmap(stats_struct);

    let mut json_str = String::from("{\n");
    for (key, value) in flat_stats.iter().sorted() {
        let new_kv = format!("  \"{}\": {},\n", key, value);
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
  "networkd.eth0.address_state": 3,
  "networkd.eth0.admin_state": 4,
  "networkd.eth0.carrier_state": 5,
  "networkd.eth0.ipv4_address_state": 3,
  "networkd.eth0.ipv6_address_state": 2,
  "networkd.eth0.oper_state": 9,
  "networkd.eth0.required_for_online": 1,
  "networkd.managed_interfaces": 1,
  "units.active_units": 0,
  "units.automount_units": 0,
  "units.device_units": 0,
  "units.failed_units": 0,
  "units.inactive_units": 0,
  "units.jobs_queued": 0,
  "units.loaded_units": 0,
  "units.masked_units": 0,
  "units.mount_units": 0,
  "units.not_found_units": 0,
  "units.path_units": 0,
  "units.scope_units": 0,
  "units.service_units": 0,
  "units.slice_units": 0,
  "units.socket_units": 0,
  "units.target_units": 0,
  "units.timer_units": 0,
  "units.total_units": 0
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
            units: crate::units::SystemdUnitStats::default(),
        }
    }

    #[test]
    fn test_flatten_hashmap() {
        let json_flat_map = flatten_hashmap(&return_monitord_stats());
        assert_eq!(26, json_flat_map.len());
    }

    #[test]
    fn test_flatten() {
        let json_flat = flatten(&return_monitord_stats());
        assert_eq!(EXPECTED_FLAT_JSON, json_flat);
        assert!(oxidized_json_checker::validate_str(&json_flat).is_ok());
    }
}
