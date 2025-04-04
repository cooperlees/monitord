//! # json module
//!
//! `json` is in charge of generating a flat BTreeMap like . serperated hierarchical
//! JSON output. This is used by some metric parsing systems when running a command.

use std::collections::BTreeMap;
use std::collections::HashMap;

use struct_field_names_as_array::FieldNamesAsArray;
use tracing::debug;

use crate::networkd;
use crate::pid1;
use crate::units;
use crate::MachineStats;
use crate::MonitordStats;

/// Add a prefix if config wants contains one
fn gen_base_metric_key(key_prefix: &String, metric_name: &str) -> String {
    match key_prefix.is_empty() {
        true => String::from(metric_name),
        false => format!("{}.{}", key_prefix, metric_name),
    }
}

fn flatten_networkd(
    networkd_stats: &networkd::NetworkdState,
    key_prefix: &String,
) -> BTreeMap<String, serde_json::Value> {
    let mut flat_stats: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let base_metric_name = gen_base_metric_key(key_prefix, &String::from("networkd"));

    let managed_interfaces_key = format!("{}.managed_interfaces", base_metric_name);
    flat_stats.insert(
        managed_interfaces_key,
        networkd_stats.managed_interfaces.into(),
    );

    if networkd_stats.interfaces_state.is_empty() {
        debug!("No networkd interfaces to add to flat JSON");
        return flat_stats;
    }

    for interface in &networkd_stats.interfaces_state {
        let interface_base = format!("{}.{}", base_metric_name, interface.name);
        flat_stats.insert(
            format!("{interface_base}.address_state"),
            (interface.address_state as u64).into(),
        );
        flat_stats.insert(
            format!("{interface_base}.admin_state"),
            (interface.admin_state as u64).into(),
        );
        flat_stats.insert(
            format!("{interface_base}.carrier_state"),
            (interface.carrier_state as u64).into(),
        );
        flat_stats.insert(
            format!("{interface_base}.ipv4_address_state"),
            (interface.ipv4_address_state as u64).into(),
        );
        flat_stats.insert(
            format!("{interface_base}.ipv6_address_state"),
            (interface.ipv6_address_state as u64).into(),
        );
        flat_stats.insert(
            format!("{interface_base}.oper_state"),
            (interface.oper_state as u64).into(),
        );
        flat_stats.insert(
            format!("{interface_base}.required_for_online"),
            (interface.required_for_online as u64).into(),
        );
    }
    flat_stats
}

fn flatten_pid1(
    optional_pid1_stats: &Option<pid1::Pid1Stats>,
    key_prefix: &String,
) -> BTreeMap<String, serde_json::Value> {
    let mut flat_stats: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    // If we're not collcting pid1 stats don't add
    let pid1_stats = match optional_pid1_stats {
        Some(ps) => ps,
        None => {
            debug!("Skipping flatenning pid1 stats as we got None ...");
            return flat_stats;
        }
    };

    let base_metric_name = gen_base_metric_key(key_prefix, &String::from("pid1"));

    flat_stats.insert(
        format!("{}.cpu_time_kernel", base_metric_name),
        pid1_stats.cpu_time_kernel.into(),
    );
    flat_stats.insert(
        format!("{}.cpu_user_kernel", base_metric_name),
        pid1_stats.cpu_time_user.into(),
    );
    flat_stats.insert(
        format!("{}.memory_usage_bytes", base_metric_name),
        pid1_stats.memory_usage_bytes.into(),
    );
    flat_stats.insert(
        format!("{}.fd_count", base_metric_name),
        pid1_stats.fd_count.into(),
    );
    flat_stats.insert(
        format!("{}.tasks", base_metric_name),
        pid1_stats.tasks.into(),
    );

    flat_stats
}

fn flatten_services(
    service_stats_hash: &HashMap<String, units::ServiceStats>,
    key_prefix: &String,
) -> BTreeMap<String, serde_json::Value> {
    let mut flat_stats: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let base_metric_name = gen_base_metric_key(key_prefix, &String::from("services"));

    for (service_name, service_stats) in service_stats_hash.iter() {
        for field_name in units::SERVICE_FIELD_NAMES {
            let key = format!("{base_metric_name}.{service_name}.{field_name}");
            match field_name.to_string().as_str() {
                "active_enter_timestamp" => {
                    flat_stats.insert(key, service_stats.active_enter_timestamp.into());
                }
                "active_exit_timestamp" => {
                    flat_stats.insert(key, service_stats.active_exit_timestamp.into());
                }
                "cpuusage_nsec" => {
                    flat_stats.insert(key, service_stats.cpuusage_nsec.into());
                }
                "inactive_exit_timestamp" => {
                    flat_stats.insert(key, service_stats.inactive_exit_timestamp.into());
                }
                "ioread_bytes" => {
                    flat_stats.insert(key, service_stats.ioread_bytes.into());
                }
                "ioread_operations" => {
                    flat_stats.insert(key, service_stats.ioread_operations.into());
                }
                "memory_available" => {
                    flat_stats.insert(key, service_stats.memory_available.into());
                }
                "memory_current" => {
                    flat_stats.insert(key, service_stats.memory_current.into());
                }
                "nrestarts" => {
                    flat_stats.insert(key, service_stats.nrestarts.into());
                }
                "processes" => {
                    flat_stats.insert(key, service_stats.processes.into());
                }
                "restart_usec" => {
                    flat_stats.insert(key, service_stats.restart_usec.into());
                }
                "state_change_timestamp" => {
                    flat_stats.insert(key, service_stats.state_change_timestamp.into());
                }
                "status_errno" => {
                    flat_stats.insert(key, service_stats.status_errno.into());
                }
                "tasks_current" => {
                    flat_stats.insert(key, service_stats.tasks_current.into());
                }
                "timeout_clean_usec" => {
                    flat_stats.insert(key, service_stats.timeout_clean_usec.into());
                }
                "watchdog_usec" => {
                    flat_stats.insert(key, service_stats.watchdog_usec.into());
                }
                _ => {
                    debug!("Got a unhandled stat: '{}'", field_name);
                }
            }
        }
    }
    flat_stats
}

fn flatten_timers(
    timer_stats_hash: &HashMap<String, crate::timer::TimerStats>,
    key_prefix: &String,
) -> BTreeMap<String, serde_json::Value> {
    let mut flat_stats: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let base_metric_name = gen_base_metric_key(key_prefix, &String::from("timers"));

    for (timer_name, timer_stats) in timer_stats_hash.iter() {
        for field_name in crate::timer::TimerStats::FIELD_NAMES_AS_ARRAY.iter() {
            let key = format!("{base_metric_name}.{timer_name}.{field_name}");
            match field_name.to_string().as_str() {
                "accuracy_usec" => {
                    flat_stats.insert(key, timer_stats.accuracy_usec.into());
                }
                "fixed_random_delay" => {
                    flat_stats.insert(key, (timer_stats.fixed_random_delay as u64).into());
                }
                "last_trigger_usec" => {
                    flat_stats.insert(key, timer_stats.last_trigger_usec.into());
                }
                "last_trigger_usec_monotonic" => {
                    flat_stats.insert(key, timer_stats.last_trigger_usec_monotonic.into());
                }
                "next_elapse_usec_monotonic" => {
                    flat_stats.insert(key, timer_stats.next_elapse_usec_monotonic.into());
                }
                "next_elapse_usec_realtime" => {
                    flat_stats.insert(key, timer_stats.next_elapse_usec_realtime.into());
                }
                "persistent" => {
                    flat_stats.insert(key, (timer_stats.persistent as u64).into());
                }
                "randomized_delay_usec" => {
                    flat_stats.insert(key, timer_stats.randomized_delay_usec.into());
                }
                "remain_after_elapse" => {
                    flat_stats.insert(key, (timer_stats.remain_after_elapse as u64).into());
                }
                "service_unit_last_state_change_usec" => {
                    flat_stats.insert(
                        key,
                        (timer_stats.service_unit_last_state_change_usec).into(),
                    );
                }
                "service_unit_last_state_change_usec_monotonic" => {
                    flat_stats.insert(
                        key,
                        (timer_stats.service_unit_last_state_change_usec_monotonic).into(),
                    );
                }
                _ => {
                    debug!("Got a unhandled stat: '{}'", field_name);
                }
            }
        }
    }
    flat_stats
}

fn flatten_unit_states(
    unit_states_hash: &HashMap<String, units::UnitStates>,
    key_prefix: &String,
) -> BTreeMap<String, serde_json::Value> {
    let mut flat_stats: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let base_metric_name = gen_base_metric_key(key_prefix, &String::from("unit_states"));

    for (unit_name, unit_state_stats) in unit_states_hash.iter() {
        for field_name in units::UNIT_STATES_FIELD_NAMES {
            let key = format!("{base_metric_name}.{unit_name}.{field_name}");
            match field_name.to_string().as_str() {
                "active_state" => {
                    flat_stats.insert(key, (unit_state_stats.active_state as u64).into());
                }
                "load_state" => {
                    flat_stats.insert(key, (unit_state_stats.load_state as u64).into());
                }
                "unhealthy" => match unit_state_stats.unhealthy {
                    false => {
                        flat_stats.insert(key, 0.into());
                    }
                    true => {
                        flat_stats.insert(key, 1.into());
                    }
                },
                "time_in_state_usecs" => {
                    flat_stats.insert(key, unit_state_stats.time_in_state_usecs.into());
                }
                _ => {
                    debug!("Got a unhandled unit state: '{}'", field_name);
                }
            }
        }
    }

    flat_stats
}

fn flatten_units(
    units_stats: &units::SystemdUnitStats,
    key_prefix: &String,
) -> BTreeMap<String, serde_json::Value> {
    // fields of the SystemdUnitStats struct we know to ignore so don't log below
    let fields_to_ignore = Vec::from(["service_stats"]);

    let mut flat_stats: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let base_metric_name = gen_base_metric_key(key_prefix, &String::from("units"));

    // TODO: Work out a smarter way to do this rather than hard code mappings
    for field_name in units::UNIT_FIELD_NAMES {
        let key = format!("{base_metric_name}.{field_name}");
        match field_name.to_string().as_str() {
            "active_units" => {
                flat_stats.insert(key, units_stats.active_units.into());
            }
            "automount_units" => {
                flat_stats.insert(key, units_stats.automount_units.into());
            }
            "device_units" => {
                flat_stats.insert(key, units_stats.device_units.into());
            }
            "failed_units" => {
                flat_stats.insert(key, units_stats.failed_units.into());
            }
            "inactive_units" => {
                flat_stats.insert(key, units_stats.inactive_units.into());
            }
            "jobs_queued" => {
                flat_stats.insert(key, units_stats.jobs_queued.into());
            }
            "loaded_units" => {
                flat_stats.insert(key, units_stats.loaded_units.into());
            }
            "masked_units" => {
                flat_stats.insert(key, units_stats.masked_units.into());
            }
            "mount_units" => {
                flat_stats.insert(key, units_stats.mount_units.into());
            }
            "not_found_units" => {
                flat_stats.insert(key, units_stats.not_found_units.into());
            }
            "path_units" => {
                flat_stats.insert(key, units_stats.path_units.into());
            }
            "scope_units" => {
                flat_stats.insert(key, units_stats.scope_units.into());
            }
            "service_units" => {
                flat_stats.insert(key, units_stats.service_units.into());
            }
            "slice_units" => {
                flat_stats.insert(key, units_stats.slice_units.into());
            }
            "socket_units" => {
                flat_stats.insert(key, units_stats.socket_units.into());
            }
            "target_units" => {
                flat_stats.insert(key, units_stats.target_units.into());
            }
            "timer_units" => {
                flat_stats.insert(key, units_stats.timer_units.into());
            }
            "timer_persistent_units" => {
                flat_stats.insert(key, units_stats.timer_persistent_units.into());
            }
            "timer_remain_after_elapse" => {
                flat_stats.insert(key, units_stats.timer_remain_after_elapse.into());
            }
            "total_units" => {
                flat_stats.insert(key, units_stats.total_units.into());
            }
            _ => {
                if !fields_to_ignore.contains(field_name) {
                    debug!("Got a unhandled stat '{}'", field_name);
                }
            }
        };
    }
    flat_stats
}

fn flatten_machines(
    machines_stats: &HashMap<String, MachineStats>,
    key_prefix: &String,
) -> BTreeMap<String, serde_json::Value> {
    let mut flat_stats = BTreeMap::new();

    if machines_stats.is_empty() {
        return flat_stats;
    }

    for (machine, stats) in machines_stats {
        let machine_key_prefix = match key_prefix.is_empty() {
            true => format!("machines.{}", machine),
            false => format!("{}.machines.{}", key_prefix, machine),
        };
        flat_stats.extend(flatten_networkd(&stats.networkd, &machine_key_prefix));
        flat_stats.extend(flatten_units(&stats.units, &machine_key_prefix));
        flat_stats.extend(flatten_pid1(&stats.pid1, &machine_key_prefix));
        flat_stats.insert(
            gen_base_metric_key(&machine_key_prefix, &String::from("system-state")),
            (stats.system_state as u64).into(),
        );
        flat_stats.extend(flatten_services(
            &stats.units.service_stats,
            &machine_key_prefix,
        ));
        flat_stats.extend(flatten_timers(
            &stats.units.timer_stats,
            &machine_key_prefix,
        ));
    }

    flat_stats
}

/// Take the standard returned structs and move all to a flat BTreeMap<str, float|int> like JSON
fn flatten_stats(
    stats_struct: &MonitordStats,
    key_prefix: &String,
) -> BTreeMap<String, serde_json::Value> {
    let mut flat_stats: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    flat_stats.extend(flatten_networkd(&stats_struct.networkd, key_prefix));
    flat_stats.extend(flatten_pid1(&stats_struct.pid1, key_prefix));
    flat_stats.insert(
        gen_base_metric_key(key_prefix, &String::from("system-state")),
        (stats_struct.system_state as u64).into(),
    );
    flat_stats.extend(flatten_services(
        &stats_struct.units.service_stats,
        key_prefix,
    ));
    flat_stats.extend(flatten_timers(&stats_struct.units.timer_stats, key_prefix));
    flat_stats.extend(flatten_unit_states(
        &stats_struct.units.unit_states,
        key_prefix,
    ));
    flat_stats.extend(flatten_units(&stats_struct.units, key_prefix));
    flat_stats.insert(
        gen_base_metric_key(key_prefix, &String::from("version")),
        stats_struct.version.to_string().into(),
    );
    flat_stats.extend(flatten_machines(&stats_struct.machines, key_prefix));
    flat_stats
}

/// Take the standard returned structs and move all to a flat JSON str
pub fn flatten(
    stats_struct: &MonitordStats,
    key_prefix: &String,
) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&flatten_stats(stats_struct, key_prefix))
}

#[cfg(test)]
mod tests {
    use crate::timer;

    use super::*;

    // This will always be sorted / deterministic ...
    const EXPECTED_FLAT_JSON: &str = r###"{
  "machines.foo.networkd.managed_interfaces": 0,
  "machines.foo.system-state": 0,
  "machines.foo.timers.unittest.timer.accuracy_usec": 69,
  "machines.foo.timers.unittest.timer.fixed_random_delay": 1,
  "machines.foo.timers.unittest.timer.last_trigger_usec": 69,
  "machines.foo.timers.unittest.timer.last_trigger_usec_monotonic": 69,
  "machines.foo.timers.unittest.timer.next_elapse_usec_monotonic": 69,
  "machines.foo.timers.unittest.timer.next_elapse_usec_realtime": 69,
  "machines.foo.timers.unittest.timer.persistent": 0,
  "machines.foo.timers.unittest.timer.randomized_delay_usec": 69,
  "machines.foo.timers.unittest.timer.remain_after_elapse": 1,
  "machines.foo.timers.unittest.timer.service_unit_last_state_change_usec": 69,
  "machines.foo.timers.unittest.timer.service_unit_last_state_change_usec_monotonic": 69,
  "machines.foo.units.active_units": 0,
  "machines.foo.units.automount_units": 0,
  "machines.foo.units.device_units": 0,
  "machines.foo.units.failed_units": 0,
  "machines.foo.units.inactive_units": 0,
  "machines.foo.units.jobs_queued": 0,
  "machines.foo.units.loaded_units": 0,
  "machines.foo.units.masked_units": 0,
  "machines.foo.units.mount_units": 0,
  "machines.foo.units.not_found_units": 0,
  "machines.foo.units.path_units": 0,
  "machines.foo.units.scope_units": 0,
  "machines.foo.units.service_units": 0,
  "machines.foo.units.slice_units": 0,
  "machines.foo.units.socket_units": 0,
  "machines.foo.units.target_units": 0,
  "machines.foo.units.timer_persistent_units": 0,
  "machines.foo.units.timer_remain_after_elapse": 0,
  "machines.foo.units.timer_units": 0,
  "machines.foo.units.total_units": 0,
  "networkd.eth0.address_state": 3,
  "networkd.eth0.admin_state": 4,
  "networkd.eth0.carrier_state": 5,
  "networkd.eth0.ipv4_address_state": 3,
  "networkd.eth0.ipv6_address_state": 2,
  "networkd.eth0.oper_state": 9,
  "networkd.eth0.required_for_online": 1,
  "networkd.managed_interfaces": 1,
  "pid1.cpu_time_kernel": 69,
  "pid1.cpu_user_kernel": 69,
  "pid1.fd_count": 69,
  "pid1.memory_usage_bytes": 69,
  "pid1.tasks": 1,
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
  "system-state": 3,
  "timers.unittest.timer.accuracy_usec": 69,
  "timers.unittest.timer.fixed_random_delay": 1,
  "timers.unittest.timer.last_trigger_usec": 69,
  "timers.unittest.timer.last_trigger_usec_monotonic": 69,
  "timers.unittest.timer.next_elapse_usec_monotonic": 69,
  "timers.unittest.timer.next_elapse_usec_realtime": 69,
  "timers.unittest.timer.persistent": 0,
  "timers.unittest.timer.randomized_delay_usec": 69,
  "timers.unittest.timer.remain_after_elapse": 1,
  "timers.unittest.timer.service_unit_last_state_change_usec": 69,
  "timers.unittest.timer.service_unit_last_state_change_usec_monotonic": 69,
  "unit_states.nvme\\x2dWDC_CL_SN730_SDBQNTY\\x2d512G\\x2d2020_37222H80070511\\x2dpart3.device.active_state": 1,
  "unit_states.nvme\\x2dWDC_CL_SN730_SDBQNTY\\x2d512G\\x2d2020_37222H80070511\\x2dpart3.device.load_state": 1,
  "unit_states.nvme\\x2dWDC_CL_SN730_SDBQNTY\\x2d512G\\x2d2020_37222H80070511\\x2dpart3.device.time_in_state_usecs": 69,
  "unit_states.nvme\\x2dWDC_CL_SN730_SDBQNTY\\x2d512G\\x2d2020_37222H80070511\\x2dpart3.device.unhealthy": 0,
  "unit_states.unittest.service.active_state": 1,
  "unit_states.unittest.service.load_state": 1,
  "unit_states.unittest.service.time_in_state_usecs": 69,
  "unit_states.unittest.service.unhealthy": 0,
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
  "units.timer_persistent_units": 0,
  "units.timer_remain_after_elapse": 0,
  "units.timer_units": 0,
  "units.total_units": 0,
  "version": "255.7-1.fc40"
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
            pid1: Some(crate::pid1::Pid1Stats {
                cpu_time_kernel: 69,
                cpu_time_user: 69,
                memory_usage_bytes: 69,
                fd_count: 69,
                tasks: 1,
            }),
            system_state: crate::system::SystemdSystemState::running,
            units: crate::units::SystemdUnitStats::default(),
            version: String::from("255.7-1.fc40")
                .try_into()
                .expect("Unable to make SystemdVersion struct"),
            machines: HashMap::from([(String::from("foo"), MachineStats::default())]),
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
        stats.units.unit_states.insert(
            String::from("unittest.service"),
            units::UnitStates {
                active_state: units::SystemdUnitActiveState::active,
                load_state: units::SystemdUnitLoadState::loaded,
                unhealthy: false,
                time_in_state_usecs: 69,
            },
        );
        let timer_unit = String::from("unittest.timer");
        let timer_stats = timer::TimerStats {
            accuracy_usec: 69,
            fixed_random_delay: true,
            last_trigger_usec: 69,
            last_trigger_usec_monotonic: 69,
            next_elapse_usec_monotonic: 69,
            next_elapse_usec_realtime: 69,
            persistent: false,
            randomized_delay_usec: 69,
            remain_after_elapse: true,
            service_unit_last_state_change_usec: 69,
            service_unit_last_state_change_usec_monotonic: 69,
        };
        stats
            .units
            .timer_stats
            .insert(timer_unit.clone(), timer_stats.clone());
        stats
            .machines
            .get_mut("foo")
            .expect("No machine foo? WTF")
            .units
            .timer_stats
            .insert(timer_unit, timer_stats);
        // Ensure we escape keys correctly
        stats.units.unit_states.insert(
            String::from(
                r"nvme\x2dWDC_CL_SN730_SDBQNTY\x2d512G\x2d2020_37222H80070511\x2dpart3.device",
            ),
            units::UnitStates {
                active_state: units::SystemdUnitActiveState::active,
                load_state: units::SystemdUnitLoadState::loaded,
                unhealthy: false,
                time_in_state_usecs: 69,
            },
        );
        stats
    }

    #[test]
    fn test_flatten_map() {
        let json_flat_map = flatten_stats(
            &return_monitord_stats(),
            &String::from("JSON serialize failed"),
        );
        assert_eq!(103, json_flat_map.len());
    }

    #[test]
    fn test_flatten() {
        let json_flat =
            flatten(&return_monitord_stats(), &String::from("")).expect("JSON serialize failed");
        assert_eq!(EXPECTED_FLAT_JSON, json_flat);
    }

    #[test]
    fn test_flatten_prefixed() {
        let json_flat = flatten(&return_monitord_stats(), &String::from("monitord"))
            .expect("JSON serialize failed");
        let json_flat_unserialized: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&json_flat).expect("JSON from_str failed");
        for (key, _value) in json_flat_unserialized.iter() {
            assert!(key.starts_with("monitord."));
        }
    }
}
