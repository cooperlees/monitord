use std::collections::HashMap;

use itertools::Itertools;
use log::debug;

use crate::networkd;
use crate::units;
use crate::MonitordStats;

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum JsonFlatValue {
    U64(u64),
    I32(i32),
    U32(u32),
}

fn gen_base_metric_key(key_prefix: &String, metric_name: &str) -> String {
    match key_prefix.len() {
        0 => String::from(metric_name),
        _ => format!("{}.{}", key_prefix, metric_name),
    }
}

fn flatten_networkd(
    networkd_stats: &networkd::NetworkdState,
    key_prefix: &String,
) -> HashMap<String, JsonFlatValue> {
    let mut flat_stats: HashMap<String, JsonFlatValue> = HashMap::new();
    let base_metric_name = gen_base_metric_key(key_prefix, &String::from("networkd"));

    let managed_interfaces_key = format!("{}.managed_interfaces", base_metric_name);
    flat_stats.insert(
        managed_interfaces_key,
        JsonFlatValue::U64(networkd_stats.managed_interfaces),
    );

    if networkd_stats.interfaces_state.is_empty() {
        debug!("No networkd interfaces to add to flat JSON");
        return flat_stats;
    }

    for interface in &networkd_stats.interfaces_state {
        let interface_base = format!("{}.{}", base_metric_name, interface.name);
        flat_stats.insert(
            format!("{interface_base}.address_state"),
            JsonFlatValue::U64(interface.address_state as u64),
        );
        flat_stats.insert(
            format!("{interface_base}.admin_state"),
            JsonFlatValue::U64(interface.admin_state as u64),
        );
        flat_stats.insert(
            format!("{interface_base}.carrier_state"),
            JsonFlatValue::U64(interface.carrier_state as u64),
        );
        flat_stats.insert(
            format!("{interface_base}.ipv4_address_state"),
            JsonFlatValue::U64(interface.ipv4_address_state as u64),
        );
        flat_stats.insert(
            format!("{interface_base}.ipv6_address_state"),
            JsonFlatValue::U64(interface.ipv6_address_state as u64),
        );
        flat_stats.insert(
            format!("{interface_base}.oper_state"),
            JsonFlatValue::U64(interface.oper_state as u64),
        );
        flat_stats.insert(
            format!("{interface_base}.required_for_online"),
            JsonFlatValue::U64(interface.required_for_online as u64),
        );
    }
    flat_stats
}

fn flatten_services(
    service_stats_hash: &HashMap<String, units::ServiceStats>,
    key_prefix: &String,
) -> HashMap<String, JsonFlatValue> {
    let mut flat_stats: HashMap<String, JsonFlatValue> = HashMap::new();
    let base_metric_name = gen_base_metric_key(key_prefix, &String::from("services"));

    for (service_name, service_stats) in service_stats_hash.iter() {
        for field_name in units::SERVICE_FIELD_NAMES {
            let key = format!("{base_metric_name}.{service_name}.{field_name}");
            match field_name.to_string().as_str() {
                "active_enter_timestamp" => {
                    flat_stats.insert(
                        key,
                        JsonFlatValue::U64(service_stats.active_enter_timestamp),
                    );
                }
                "active_exit_timestamp" => {
                    flat_stats.insert(key, JsonFlatValue::U64(service_stats.active_exit_timestamp));
                }
                "cpuusage_nsec" => {
                    flat_stats.insert(key, JsonFlatValue::U64(service_stats.cpuusage_nsec));
                }
                "inactive_exit_timestamp" => {
                    flat_stats.insert(
                        key,
                        JsonFlatValue::U64(service_stats.inactive_exit_timestamp),
                    );
                }
                "ioread_bytes" => {
                    flat_stats.insert(key, JsonFlatValue::U64(service_stats.ioread_bytes));
                }
                "ioread_operations" => {
                    flat_stats.insert(key, JsonFlatValue::U64(service_stats.ioread_operations));
                }
                "memory_available" => {
                    flat_stats.insert(key, JsonFlatValue::U64(service_stats.memory_available));
                }
                "memory_current" => {
                    flat_stats.insert(key, JsonFlatValue::U64(service_stats.memory_current));
                }
                "nrestarts" => {
                    flat_stats.insert(key, JsonFlatValue::U32(service_stats.nrestarts));
                }
                "processes" => {
                    flat_stats.insert(key, JsonFlatValue::U32(service_stats.processes));
                }
                "restart_usec" => {
                    flat_stats.insert(key, JsonFlatValue::U64(service_stats.restart_usec));
                }
                "state_change_timestamp" => {
                    flat_stats.insert(
                        key,
                        JsonFlatValue::U64(service_stats.state_change_timestamp),
                    );
                }
                "status_errno" => {
                    flat_stats.insert(key, JsonFlatValue::I32(service_stats.status_errno));
                }
                "tasks_current" => {
                    flat_stats.insert(key, JsonFlatValue::U64(service_stats.tasks_current));
                }
                "timeout_clean_usec" => {
                    flat_stats.insert(key, JsonFlatValue::U64(service_stats.timeout_clean_usec));
                }
                "watchdog_usec" => {
                    flat_stats.insert(key, JsonFlatValue::U64(service_stats.watchdog_usec));
                }
                _ => {
                    debug!("Got a unhandled stat '{}'", field_name);
                }
            }
        }
    }
    flat_stats
}

fn flatten_units(
    units_stats: &units::SystemdUnitStats,
    key_prefix: &String,
) -> HashMap<String, JsonFlatValue> {
    let mut flat_stats: HashMap<String, JsonFlatValue> = HashMap::new();
    let base_metric_name = gen_base_metric_key(key_prefix, &String::from("units"));

    // TODO: Work out a smarter way to do this rather than hard code mappings
    for field_name in units::UNIT_FIELD_NAMES {
        let key = format!("{base_metric_name}.{field_name}");
        match field_name.to_string().as_str() {
            "active_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.active_units));
            }
            "automount_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.automount_units));
            }
            "device_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.device_units));
            }
            "failed_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.failed_units));
            }
            "inactive_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.inactive_units));
            }
            "jobs_queued" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.jobs_queued));
            }
            "loaded_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.loaded_units));
            }
            "masked_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.masked_units));
            }
            "mount_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.mount_units));
            }
            "not_found_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.not_found_units));
            }
            "path_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.path_units));
            }
            "scope_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.scope_units));
            }
            "service_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.service_units));
            }
            "slice_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.slice_units));
            }
            "socket_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.socket_units));
            }
            "target_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.target_units));
            }
            "timer_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.timer_units));
            }
            "total_units" => {
                flat_stats.insert(key, JsonFlatValue::U64(units_stats.total_units));
            }
            _ => {
                debug!("Got a unhandled stat '{}'", field_name);
            }
        };
    }
    flat_stats
}

/// Take the standard returned structs and move all to a flat HashMap<str, float|int> like JSON
pub fn flatten_hashmap(
    stats_struct: &MonitordStats,
    key_prefix: &String,
) -> HashMap<String, JsonFlatValue> {
    let mut flat_stats: HashMap<String, JsonFlatValue> = HashMap::new();
    flat_stats.extend(flatten_networkd(&stats_struct.networkd, key_prefix));
    flat_stats.extend(flatten_services(
        &stats_struct.units.service_stats,
        key_prefix,
    ));
    flat_stats.extend(flatten_units(&stats_struct.units, key_prefix));
    flat_stats
}

/// Take the standard returned structs and move all to a flat JSON str
pub fn flatten(stats_struct: &MonitordStats, key_prefix: &String) -> String {
    let flat_stats = flatten_hashmap(stats_struct, key_prefix);

    let mut json_str = String::from("{\n");
    for (key, value) in flat_stats.iter().sorted() {
        let new_kv_a = format!("  \"{}\": ", key);
        let new_kv = match value {
            JsonFlatValue::I32(an_int) => {
                format!("{}{},\n", new_kv_a, an_int)
            }
            JsonFlatValue::U32(an_int) => {
                format!("{}{},\n", new_kv_a, an_int)
            }
            JsonFlatValue::U64(an_int) => {
                format!("{}{},\n", new_kv_a, an_int)
            }
        };
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
  "services.unittest.service.active_enter_timestamp": 0,
  "services.unittest.service.active_exit_timestamp": 0,
  "services.unittest.service.cpuusage_nsec": 0,
  "services.unittest.service.inactive_exit_timestamp": 0,
  "services.unittest.service.ioread_bytes": 0,
  "services.unittest.service.ioread_operations": 0,
  "services.unittest.service.memory_available": 0,
  "services.unittest.service.memory_current": 0,
  "services.unittest.service.nrestarts": 0,
  "services.unittest.service.processes": 0,
  "services.unittest.service.restart_usec": 0,
  "services.unittest.service.state_change_timestamp": 0,
  "services.unittest.service.status_errno": -69,
  "services.unittest.service.tasks_current": 0,
  "services.unittest.service.timeout_clean_usec": 0,
  "services.unittest.service.watchdog_usec": 0,
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

    const EXPECTED_PREFIXED_FLAT_JSON: &str = r###"{
  "monitord.networkd.eth0.address_state": 3,
  "monitord.networkd.eth0.admin_state": 4,
  "monitord.networkd.eth0.carrier_state": 5,
  "monitord.networkd.eth0.ipv4_address_state": 3,
  "monitord.networkd.eth0.ipv6_address_state": 2,
  "monitord.networkd.eth0.oper_state": 9,
  "monitord.networkd.eth0.required_for_online": 1,
  "monitord.networkd.managed_interfaces": 1,
  "monitord.services.unittest.service.active_enter_timestamp": 0,
  "monitord.services.unittest.service.active_exit_timestamp": 0,
  "monitord.services.unittest.service.cpuusage_nsec": 0,
  "monitord.services.unittest.service.inactive_exit_timestamp": 0,
  "monitord.services.unittest.service.ioread_bytes": 0,
  "monitord.services.unittest.service.ioread_operations": 0,
  "monitord.services.unittest.service.memory_available": 0,
  "monitord.services.unittest.service.memory_current": 0,
  "monitord.services.unittest.service.nrestarts": 0,
  "monitord.services.unittest.service.processes": 0,
  "monitord.services.unittest.service.restart_usec": 0,
  "monitord.services.unittest.service.state_change_timestamp": 0,
  "monitord.services.unittest.service.status_errno": -69,
  "monitord.services.unittest.service.tasks_current": 0,
  "monitord.services.unittest.service.timeout_clean_usec": 0,
  "monitord.services.unittest.service.watchdog_usec": 0,
  "monitord.units.active_units": 0,
  "monitord.units.automount_units": 0,
  "monitord.units.device_units": 0,
  "monitord.units.failed_units": 0,
  "monitord.units.inactive_units": 0,
  "monitord.units.jobs_queued": 0,
  "monitord.units.loaded_units": 0,
  "monitord.units.masked_units": 0,
  "monitord.units.mount_units": 0,
  "monitord.units.not_found_units": 0,
  "monitord.units.path_units": 0,
  "monitord.units.scope_units": 0,
  "monitord.units.service_units": 0,
  "monitord.units.slice_units": 0,
  "monitord.units.socket_units": 0,
  "monitord.units.target_units": 0,
  "monitord.units.timer_units": 0,
  "monitord.units.total_units": 0
}"###;

    fn return_monitord_stats() -> MonitordStats {
        let mut stats = MonitordStats {
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
        };
        let service_unit_name = String::from("unittest.service");
        stats.units.service_stats.insert(
            service_unit_name.clone(),
            units::ServiceStats {
                // Ensure json-flat handles negative i32s
                status_errno: -69,
                ..Default::default()
            },
        );
        stats
    }

    #[test]
    fn test_flatten_hashmap() {
        let json_flat_map = flatten_hashmap(&return_monitord_stats(), &String::from(""));
        assert_eq!(42, json_flat_map.len());
    }

    #[test]
    fn test_flatten() {
        let json_flat = flatten(&return_monitord_stats(), &String::from(""));
        assert_eq!(EXPECTED_FLAT_JSON, json_flat);
        assert!(oxidized_json_checker::validate_str(&json_flat).is_ok());
    }

    #[test]
    fn test_flatten_prefixed() {
        let json_flat = flatten(&return_monitord_stats(), &String::from("monitord"));
        assert_eq!(EXPECTED_PREFIXED_FLAT_JSON, json_flat);
        assert!(oxidized_json_checker::validate_str(&json_flat).is_ok());
    }
}
