//! # json module
//!
//! `json` is in charge of generating a flat BTreeMap like . serperated hierarchical
//! JSON output. This is used by some metric parsing systems when running a command.

use std::collections::BTreeMap;
use std::collections::HashMap;

use tracing::debug;

use crate::dbus_stats;
use crate::networkd;
use crate::pid1;
use crate::units;
use crate::MachineStats;
use crate::MonitordStats;

/// Add a prefix if the config specifies one
fn gen_base_metric_key(key_prefix: &str, metric_name: &str) -> String {
    match key_prefix.is_empty() {
        true => String::from(metric_name),
        false => format!("{}.{}", key_prefix, metric_name),
    }
}

fn flatten_networkd(
    networkd_stats: &networkd::NetworkdState,
    key_prefix: &str,
) -> Vec<(String, serde_json::Value)> {
    let mut flat_stats = vec![];
    let base_metric_name = gen_base_metric_key(key_prefix, "networkd");

    let managed_interfaces_key = format!("{}.managed_interfaces", base_metric_name);
    flat_stats.push((
        managed_interfaces_key,
        networkd_stats.managed_interfaces.into(),
    ));

    if networkd_stats.interfaces_state.is_empty() {
        debug!("No networkd interfaces to add to flat JSON");
        return flat_stats;
    }

    for interface in &networkd_stats.interfaces_state {
        let interface_base = format!("{}.{}", base_metric_name, interface.name);
        flat_stats.push((
            format!("{interface_base}.address_state"),
            (interface.address_state as u64).into(),
        ));
        flat_stats.push((
            format!("{interface_base}.admin_state"),
            (interface.admin_state as u64).into(),
        ));
        flat_stats.push((
            format!("{interface_base}.carrier_state"),
            (interface.carrier_state as u64).into(),
        ));
        flat_stats.push((
            format!("{interface_base}.ipv4_address_state"),
            (interface.ipv4_address_state as u64).into(),
        ));
        flat_stats.push((
            format!("{interface_base}.ipv6_address_state"),
            (interface.ipv6_address_state as u64).into(),
        ));
        flat_stats.push((
            format!("{interface_base}.oper_state"),
            (interface.oper_state as u64).into(),
        ));
        flat_stats.push((
            format!("{interface_base}.required_for_online"),
            (interface.required_for_online as u64).into(),
        ));
    }
    flat_stats
}

fn flatten_pid1(
    optional_pid1_stats: &Option<pid1::Pid1Stats>,
    key_prefix: &str,
) -> Vec<(String, serde_json::Value)> {
    // If we're not collecting pid1 stats don't add
    let pid1_stats = match optional_pid1_stats {
        Some(ps) => ps,
        None => {
            debug!("Skipping flattening pid1 stats as we got None ...");
            return Vec::new();
        }
    };

    let base_metric_name = gen_base_metric_key(key_prefix, "pid1");

    vec![
        (
            format!("{}.cpu_time_kernel", base_metric_name),
            pid1_stats.cpu_time_kernel.into(),
        ),
        (
            format!("{}.cpu_user_kernel", base_metric_name),
            pid1_stats.cpu_time_user.into(),
        ),
        (
            format!("{}.memory_usage_bytes", base_metric_name),
            pid1_stats.memory_usage_bytes.into(),
        ),
        (
            format!("{}.fd_count", base_metric_name),
            pid1_stats.fd_count.into(),
        ),
        (
            format!("{}.tasks", base_metric_name),
            pid1_stats.tasks.into(),
        ),
    ]
}

fn flatten_unit_files_scope(
    scope: &units::UnitFilesScope,
    base: &str,
) -> Vec<(String, serde_json::Value)> {
    let mut flat_stats = Vec::new();
    for (unit_type, count) in &scope.generated {
        flat_stats.push((
            format!("{base}.generated.{unit_type}_units"),
            (*count).into(),
        ));
    }
    for (unit_type, count) in &scope.transient {
        flat_stats.push((
            format!("{base}.transient.{unit_type}_units"),
            (*count).into(),
        ));
    }
    flat_stats
}

fn flatten_unit_files(
    unit_files: &units::UnitFilesStats,
    key_prefix: &str,
) -> Vec<(String, serde_json::Value)> {
    let base = gen_base_metric_key(key_prefix, "unit_files");
    let mut flat_stats = flatten_unit_files_scope(&unit_files.root, &format!("{base}.root"));
    flat_stats.extend(flatten_unit_files_scope(
        &unit_files.user,
        &format!("{base}.user"),
    ));
    flat_stats
}

fn flatten_services(
    service_stats_hash: &HashMap<String, units::ServiceStats>,
    key_prefix: &str,
) -> Vec<(String, serde_json::Value)> {
    let mut flat_stats = Vec::new();
    let base_metric_name = gen_base_metric_key(key_prefix, "services");

    for (service_name, service_stats) in service_stats_hash.iter() {
        if let Ok(serde_json::Value::Object(map)) = serde_json::to_value(service_stats) {
            for (field_name, value) in map {
                if value.is_number() {
                    let key = format!("{base_metric_name}.{service_name}.{field_name}");
                    flat_stats.push((key, value));
                }
            }
        }
    }
    flat_stats
}

fn flatten_timers(
    timer_stats_hash: &HashMap<String, crate::timer::TimerStats>,
    key_prefix: &str,
) -> Vec<(String, serde_json::Value)> {
    let mut flat_stats = Vec::new();
    let base_metric_name = gen_base_metric_key(key_prefix, "timers");

    for (timer_name, timer_stats) in timer_stats_hash.iter() {
        if let Ok(serde_json::Value::Object(map)) = serde_json::to_value(timer_stats) {
            for (field_name, value) in map {
                let key = format!("{base_metric_name}.{timer_name}.{field_name}");
                if value.is_number() {
                    flat_stats.push((key, value));
                } else if let Some(b) = value.as_bool() {
                    flat_stats.push((key, (b as u64).into()));
                }
            }
        }
    }
    flat_stats
}

fn flatten_unit_states(
    unit_states_hash: &HashMap<String, units::UnitStates>,
    key_prefix: &str,
) -> Vec<(String, serde_json::Value)> {
    let mut flat_stats = Vec::new();
    let base_metric_name = gen_base_metric_key(key_prefix, "unit_states");

    for (unit_name, unit_state_stats) in unit_states_hash.iter() {
        if let Ok(serde_json::Value::Object(map)) = serde_json::to_value(unit_state_stats) {
            for (field_name, value) in map {
                let key = format!("{base_metric_name}.{unit_name}.{field_name}");
                if value.is_number() {
                    flat_stats.push((key, value));
                } else if let Some(b) = value.as_bool() {
                    flat_stats.push((key, (b as u64).into()));
                }
            }
        }
    }

    flat_stats
}

/// Lightweight view of `SystemdUnitStats` containing only the numeric counters.
/// Used by `flatten_units` to avoid serializing the nested `service_stats`,
/// `timer_stats`, and `unit_states` hashmaps, keeping flattening O(number_of_counters).
#[derive(serde::Serialize)]
struct UnitCounters {
    activating_units: u64,
    active_units: u64,
    automount_units: u64,
    device_units: u64,
    failed_units: u64,
    inactive_units: u64,
    jobs_queued: u64,
    loaded_units: u64,
    masked_units: u64,
    mount_units: u64,
    not_found_units: u64,
    path_units: u64,
    scope_units: u64,
    service_units: u64,
    slice_units: u64,
    socket_units: u64,
    target_units: u64,
    timer_units: u64,
    timer_persistent_units: u64,
    timer_remain_after_elapse: u64,
    total_units: u64,
}

impl From<&units::SystemdUnitStats> for UnitCounters {
    fn from(s: &units::SystemdUnitStats) -> Self {
        Self {
            activating_units: s.activating_units,
            active_units: s.active_units,
            automount_units: s.automount_units,
            device_units: s.device_units,
            failed_units: s.failed_units,
            inactive_units: s.inactive_units,
            jobs_queued: s.jobs_queued,
            loaded_units: s.loaded_units,
            masked_units: s.masked_units,
            mount_units: s.mount_units,
            not_found_units: s.not_found_units,
            path_units: s.path_units,
            scope_units: s.scope_units,
            service_units: s.service_units,
            slice_units: s.slice_units,
            socket_units: s.socket_units,
            target_units: s.target_units,
            timer_units: s.timer_units,
            timer_persistent_units: s.timer_persistent_units,
            timer_remain_after_elapse: s.timer_remain_after_elapse,
            total_units: s.total_units,
        }
    }
}

fn flatten_units(
    units_stats: &units::SystemdUnitStats,
    key_prefix: &str,
) -> Vec<(String, serde_json::Value)> {
    let mut flat_stats = Vec::new();
    let base_metric_name = gen_base_metric_key(key_prefix, "units");

    if let Ok(serde_json::Value::Object(map)) =
        serde_json::to_value(UnitCounters::from(units_stats))
    {
        for (field_name, value) in map {
            if value.is_number() {
                let key = format!("{base_metric_name}.{field_name}");
                flat_stats.push((key, value));
            }
        }
    }
    flat_stats
}

fn flatten_machines(
    machines_stats: &HashMap<String, MachineStats>,
    key_prefix: &str,
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
        flat_stats.extend(flatten_unit_files(
            &stats.units.unit_files,
            &machine_key_prefix,
        ));
        flat_stats.extend(flatten_units_collection_timings(
            &stats.units.collection_timings,
            &machine_key_prefix,
        ));
        flat_stats.extend(flatten_pid1(&stats.pid1, &machine_key_prefix));
        flat_stats.insert(
            gen_base_metric_key(&machine_key_prefix, "system-state"),
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
        flat_stats.extend(flatten_boot_blame(&stats.boot_blame, &machine_key_prefix));
        flat_stats.extend(flatten_verify_stats(
            &stats.verify_stats,
            &machine_key_prefix,
        ));
    }

    flat_stats
}

fn flatten_dbus_stats(
    optional_dbus_stats: &Option<dbus_stats::DBusStats>,
    key_prefix: &str,
) -> BTreeMap<String, serde_json::Value> {
    let mut flat_stats: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let dbus_stats = match optional_dbus_stats {
        Some(ds) => ds,
        None => {
            debug!("Skipping flattening dbus stats as we got None ...");
            return flat_stats;
        }
    };

    let base_metric_name = gen_base_metric_key(key_prefix, "dbus");
    let fields = [
        // ignore serial
        ("active_connections", dbus_stats.active_connections),
        ("incomplete_connections", dbus_stats.incomplete_connections),
        ("bus_names", dbus_stats.bus_names),
        ("peak_bus_names", dbus_stats.peak_bus_names),
        (
            "peak_bus_names_per_connection",
            dbus_stats.peak_bus_names_per_connection,
        ),
        ("match_rules", dbus_stats.match_rules),
        ("peak_match_rules", dbus_stats.peak_match_rules),
        (
            "peak_match_rules_per_connection",
            dbus_stats.peak_match_rules_per_connection,
        ),
    ];

    for (field_name, value) in fields {
        if let Some(val) = value {
            flat_stats.insert(format!("{base_metric_name}.{field_name}"), val.into());
        }
    }

    if let Some(peer_accounting) = dbus_stats.peer_accounting() {
        for peer in peer_accounting.values() {
            let peer_name = peer.get_name();
            let peer_fields = [
                ("name_objects", peer.name_objects),
                ("match_bytes", peer.match_bytes),
                ("matches", peer.matches),
                ("reply_objects", peer.reply_objects),
                ("incoming_bytes", peer.incoming_bytes),
                ("incoming_fds", peer.incoming_fds),
                ("outgoing_bytes", peer.outgoing_bytes),
                ("outgoing_fds", peer.outgoing_fds),
                ("activation_request_bytes", peer.activation_request_bytes),
                ("activation_request_fds", peer.activation_request_fds),
            ];

            for (field_name, value) in peer_fields {
                if let Some(val) = value {
                    flat_stats.insert(
                        format!("{base_metric_name}.peer.{peer_name}.{field_name}"),
                        val.into(),
                    );
                }
            }
        }
    }

    if let Some(cgroup_accounting) = dbus_stats.cgroup_accounting() {
        for cgroup in cgroup_accounting.values() {
            let cgroup_name = &cgroup.name;
            let cgroup_fields = [
                ("name_objects", cgroup.name_objects),
                ("match_bytes", cgroup.match_bytes),
                ("matches", cgroup.matches),
                ("reply_objects", cgroup.reply_objects),
                ("incoming_bytes", cgroup.incoming_bytes),
                ("incoming_fds", cgroup.incoming_fds),
                ("outgoing_bytes", cgroup.outgoing_bytes),
                ("outgoing_fds", cgroup.outgoing_fds),
                ("activation_request_bytes", cgroup.activation_request_bytes),
                ("activation_request_fds", cgroup.activation_request_fds),
            ];

            for (field_name, value) in cgroup_fields {
                if let Some(val) = value {
                    flat_stats.insert(
                        format!("{base_metric_name}.cgroup.{cgroup_name}.{field_name}"),
                        val.into(),
                    );
                }
            }
        }
    }

    if let Some(user_accounting) = dbus_stats.user_accounting() {
        // process user accounting if present
        for user in user_accounting.values() {
            let user_name = &user.username;
            let user_fields = [
                ("bytes", user.bytes.clone()),
                ("fds", user.fds.clone()),
                ("matches", user.matches.clone()),
                ("objects", user.objects.clone()),
            ];

            for (field_name, value) in user_fields {
                if let Some(val) = value {
                    flat_stats.insert(
                        format!("{base_metric_name}.user.{user_name}.{field_name}"),
                        val.get_usage().into(),
                    );
                }
            }
        }
    }

    flat_stats
}

fn flatten_boot_blame(
    optional_boot_blame: &Option<crate::boot::BootBlameStats>,
    key_prefix: &str,
) -> BTreeMap<String, serde_json::Value> {
    let mut flat_stats: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let boot_blame_stats = match optional_boot_blame {
        Some(bb) => bb,
        None => {
            debug!("Skipping flattening boot blame stats as we got None ...");
            return flat_stats;
        }
    };

    let base_metric_name = gen_base_metric_key(key_prefix, "boot.blame");

    for (unit_name, activation_time) in boot_blame_stats.iter() {
        let key = format!("{}.{}", base_metric_name, unit_name);
        flat_stats.insert(key, (*activation_time).into());
    }

    flat_stats
}

fn flatten_verify_stats(
    optional_verify_stats: &Option<crate::verify::VerifyStats>,
    key_prefix: &str,
) -> BTreeMap<String, serde_json::Value> {
    let mut flat_stats: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let verify_stats = match optional_verify_stats {
        Some(vs) => vs,
        None => {
            debug!("Skipping flattening verify stats as we got None ...");
            return flat_stats;
        }
    };

    let base_metric_name = gen_base_metric_key(key_prefix, "verify.failing");

    // Add total count
    flat_stats.insert(
        format!("{base_metric_name}.total"),
        verify_stats.total.into(),
    );

    // Add counts by type (only if they exist)
    for (unit_type, count) in &verify_stats.by_type {
        flat_stats.insert(format!("{base_metric_name}.{unit_type}"), (*count).into());
    }

    flat_stats
}

fn flatten_collector_timings(
    timings: &[crate::CollectorTiming],
    key_prefix: &str,
) -> BTreeMap<String, serde_json::Value> {
    let mut flat_stats: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let base_metric_name = gen_base_metric_key(key_prefix, "collector_timings");
    for t in timings {
        flat_stats.insert(
            format!("{base_metric_name}.{}.start_offset_ms", t.name),
            t.start_offset_ms.into(),
        );
        flat_stats.insert(
            format!("{base_metric_name}.{}.elapsed_ms", t.name),
            t.elapsed_ms.into(),
        );
        flat_stats.insert(
            format!("{base_metric_name}.{}.success", t.name),
            (if t.success { 1u64 } else { 0u64 }).into(),
        );
    }
    flat_stats
}

fn flatten_units_collection_timings(
    timings: &units::UnitsCollectionTimings,
    key_prefix: &str,
) -> BTreeMap<String, serde_json::Value> {
    let mut flat_stats: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let base_metric_name = gen_base_metric_key(key_prefix, "collection_timings");
    flat_stats.insert(
        format!("{base_metric_name}.list_units_ms"),
        timings.list_units_ms.into(),
    );
    flat_stats.insert(
        format!("{base_metric_name}.per_unit_loop_ms"),
        timings.per_unit_loop_ms.into(),
    );
    flat_stats.insert(
        format!("{base_metric_name}.timer_dbus_fetches"),
        timings.timer_dbus_fetches.into(),
    );
    flat_stats.insert(
        format!("{base_metric_name}.state_dbus_fetches"),
        timings.state_dbus_fetches.into(),
    );
    flat_stats.insert(
        format!("{base_metric_name}.service_dbus_fetches"),
        timings.service_dbus_fetches.into(),
    );
    flat_stats
}

/// Take the standard returned structs and move all to a flat BTreeMap<str, float|int> like JSON
fn flatten_stats(
    stats_struct: &MonitordStats,
    key_prefix: &str,
) -> BTreeMap<String, serde_json::Value> {
    let mut flat_stats: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    flat_stats.insert(
        gen_base_metric_key(key_prefix, "stat_collection_run_time_ms"),
        stats_struct.stat_collection_run_time_ms.into(),
    );
    flat_stats.extend(flatten_collector_timings(
        &stats_struct.collector_timings,
        key_prefix,
    ));
    flat_stats.extend(flatten_units_collection_timings(
        &stats_struct.units.collection_timings,
        key_prefix,
    ));
    flat_stats.extend(flatten_networkd(&stats_struct.networkd, key_prefix));
    flat_stats.extend(flatten_pid1(&stats_struct.pid1, key_prefix));
    flat_stats.insert(
        gen_base_metric_key(key_prefix, "system-state"),
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
    flat_stats.extend(flatten_unit_files(
        &stats_struct.units.unit_files,
        key_prefix,
    ));
    flat_stats.insert(
        gen_base_metric_key(key_prefix, "version"),
        stats_struct.version.to_string().into(),
    );
    flat_stats.extend(flatten_machines(&stats_struct.machines, key_prefix));
    flat_stats.extend(flatten_dbus_stats(&stats_struct.dbus_stats, key_prefix));
    flat_stats.extend(flatten_boot_blame(&stats_struct.boot_blame, key_prefix));
    flat_stats.extend(flatten_verify_stats(&stats_struct.verify_stats, key_prefix));
    flat_stats
}

/// Take the standard returned structs and move all to a flat JSON str
pub fn flatten(
    stats_struct: &MonitordStats,
    key_prefix: &str,
) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&flatten_stats(stats_struct, key_prefix))
}

#[cfg(test)]
mod tests {
    use crate::timer;

    use super::*;

    // This will always be sorted / deterministic ...
    const EXPECTED_FLAT_JSON: &str = r###"{
  "boot.blame.cpe_chef.service": 103.05,
  "boot.blame.dnf5-automatic.service": 204.159,
  "boot.blame.sys-module-fuse.device": 16.21,
  "collection_timings.list_units_ms": 5.0,
  "collection_timings.per_unit_loop_ms": 37.0,
  "collection_timings.service_dbus_fetches": 1,
  "collection_timings.state_dbus_fetches": 0,
  "collection_timings.timer_dbus_fetches": 4,
  "collector_timings.boot_blame.elapsed_ms": 12.5,
  "collector_timings.boot_blame.start_offset_ms": 0.25,
  "collector_timings.boot_blame.success": 0,
  "collector_timings.units.elapsed_ms": 42.0,
  "collector_timings.units.start_offset_ms": 0.5,
  "collector_timings.units.success": 1,
  "machines.foo.collection_timings.list_units_ms": 0.0,
  "machines.foo.collection_timings.per_unit_loop_ms": 0.0,
  "machines.foo.collection_timings.service_dbus_fetches": 0,
  "machines.foo.collection_timings.state_dbus_fetches": 0,
  "machines.foo.collection_timings.timer_dbus_fetches": 0,
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
  "machines.foo.units.activating_units": 0,
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
  "stat_collection_run_time_ms": 69.0,
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
  "unit_states.nvme\\x2dWDC_CL_SN730_SDBQNTY\\x2d512G\\x2d2020_37222H80070511\\x2dpart3.device.unhealthy": 0,
  "unit_states.unittest.service.active_state": 1,
  "unit_states.unittest.service.load_state": 1,
  "unit_states.unittest.service.time_in_state_usecs": 69,
  "unit_states.unittest.service.unhealthy": 0,
  "units.activating_units": 0,
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
  "verify.failing.service": 2,
  "verify.failing.slice": 1,
  "verify.failing.total": 3,
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
            dbus_stats: None,
            boot_blame: None,
            verify_stats: Some(crate::verify::VerifyStats {
                total: 3,
                by_type: HashMap::from([("service".to_string(), 2), ("slice".to_string(), 1)]),
            }),
            stat_collection_run_time_ms: 69.0,
            collector_timings: vec![
                crate::CollectorTiming {
                    name: "units".to_string(),
                    start_offset_ms: 0.5,
                    elapsed_ms: 42.0,
                    success: true,
                },
                crate::CollectorTiming {
                    name: "boot_blame".to_string(),
                    start_offset_ms: 0.25,
                    elapsed_ms: 12.5,
                    success: false,
                },
            ],
        };
        stats.units.collection_timings = units::UnitsCollectionTimings {
            list_units_ms: 5.0,
            per_unit_loop_ms: 37.0,
            timer_dbus_fetches: 4,
            state_dbus_fetches: 0,
            service_dbus_fetches: 1,
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
                time_in_state_usecs: Some(69),
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
                time_in_state_usecs: None,
            },
        );
        // Add boot blame stats
        let mut boot_blame = crate::boot::BootBlameStats::new();
        boot_blame.insert(String::from("dnf5-automatic.service"), 204.159);
        boot_blame.insert(String::from("cpe_chef.service"), 103.050);
        boot_blame.insert(String::from("sys-module-fuse.device"), 16.210);
        stats.boot_blame = Some(boot_blame);
        stats
    }

    #[test]
    fn test_flatten_map() {
        let json_flat_map = flatten_stats(&return_monitord_stats(), "");
        assert_eq!(127, json_flat_map.len());
    }

    #[test]
    fn test_flatten() {
        let json_flat = flatten(&return_monitord_stats(), "").expect("JSON serialize failed");
        assert_eq!(EXPECTED_FLAT_JSON, json_flat);
    }

    #[test]
    fn test_flatten_prefixed() {
        let json_flat =
            flatten(&return_monitord_stats(), "monitord").expect("JSON serialize failed");
        let json_flat_unserialized: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&json_flat).expect("JSON from_str failed");
        for (key, _value) in json_flat_unserialized.iter() {
            assert!(key.starts_with("monitord."));
        }
    }

    /// Ensure `UnitCounters` covers every scalar (non-hashmap) field of `SystemdUnitStats`.
    ///
    /// If a new counter field is added to `SystemdUnitStats` but not to `UnitCounters`
    /// (and its `From` impl), this test will fail, preventing silent omissions from the
    /// flat JSON output.
    #[test]
    fn test_unit_counters_covers_all_scalar_fields() {
        // Fields of SystemdUnitStats that are nested maps, not scalar counters.
        const NON_COUNTER_FIELDS: &[&str] = &[
            "unit_files",
            "service_stats",
            "timer_stats",
            "unit_states",
            "collection_timings",
        ];

        // Scalar counter field names expected from SystemdUnitStats.
        let expected: std::collections::BTreeSet<&str> = units::UNIT_FIELD_NAMES
            .iter()
            .copied()
            .filter(|f| !NON_COUNTER_FIELDS.contains(f))
            .collect();

        // Field names actually present in UnitCounters (via serde serialization).
        let counters_json =
            serde_json::to_value(UnitCounters::from(&units::SystemdUnitStats::default()))
                .expect("UnitCounters serialization failed");
        let actual: std::collections::BTreeSet<&str> = counters_json
            .as_object()
            .expect("UnitCounters must serialize to a JSON object")
            .keys()
            .map(|s| s.as_str())
            .collect();

        assert_eq!(
            expected,
            actual,
            "UnitCounters is out of sync with SystemdUnitStats scalar fields.\n\
             Missing from UnitCounters: {:?}\n\
             Extra in UnitCounters: {:?}",
            expected.difference(&actual).collect::<Vec<_>>(),
            actual.difference(&expected).collect::<Vec<_>>(),
        );
    }
}
