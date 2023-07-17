use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use dbus::blocking::Connection;
use struct_field_names_as_array::FieldNamesAsArray;
use tracing::debug;
use tracing::error;

#[derive(
    serde::Serialize, serde::Deserialize, Debug, Default, Eq, FieldNamesAsArray, PartialEq,
)]
pub struct SystemdUnitStats {
    pub active_units: u64,
    pub automount_units: u64,
    pub device_units: u64,
    pub failed_units: u64,
    pub inactive_units: u64,
    pub jobs_queued: u64,
    pub loaded_units: u64,
    pub masked_units: u64,
    pub mount_units: u64,
    pub not_found_units: u64,
    pub path_units: u64,
    pub scope_units: u64,
    pub service_units: u64,
    pub slice_units: u64,
    pub socket_units: u64,
    pub target_units: u64,
    pub timer_units: u64,
    pub total_units: u64,
    pub service_stats: HashMap<String, ServiceStats>,
}

#[derive(
    serde::Serialize, serde::Deserialize, Debug, Default, Eq, FieldNamesAsArray, PartialEq,
)]
pub struct ServiceStats {
    pub active_enter_timestamp: u64,
    pub active_exit_timestamp: u64,
    pub cpuusage_nsec: u64,
    pub inactive_exit_timestamp: u64,
    pub ioread_bytes: u64,
    pub ioread_operations: u64,
    pub memory_available: u64,
    pub memory_current: u64,
    pub nrestarts: u32,
    pub processes: u32,
    pub restart_usec: u64,
    pub state_change_timestamp: u64,
    pub status_errno: i32,
    pub tasks_current: u64,
    pub timeout_clean_usec: u64,
    pub watchdog_usec: u64,
}

pub const SERVICE_FIELD_NAMES: &[&str] = ServiceStats::FIELD_NAMES_AS_ARRAY;
pub const UNIT_FIELD_NAMES: &[&str] = SystemdUnitStats::FIELD_NAMES_AS_ARRAY;

fn parse_service(c: &Connection, name: &str, path: &str) -> Result<ServiceStats, dbus::Error> {
    debug!("Parsing service {} stats", name);
    let p = c.with_proxy("org.freedesktop.systemd1", path, Duration::new(2, 0));
    use crate::unit_dbus::OrgFreedesktopSystemd1Service;
    use crate::unit_dbus::OrgFreedesktopSystemd1Unit;
    Ok(ServiceStats {
        active_enter_timestamp: p.active_enter_timestamp()?,
        active_exit_timestamp: p.active_exit_timestamp()?,
        cpuusage_nsec: p.cpuusage_nsec()?,
        inactive_exit_timestamp: p.inactive_exit_timestamp()?,
        ioread_bytes: p.ioread_bytes()?,
        ioread_operations: p.ioread_operations()?,
        memory_current: p.memory_current()?,
        memory_available: p.memory_available()?,
        nrestarts: p.nrestarts()?,
        processes: p.get_processes()?[0].1,
        restart_usec: p.restart_usec()?,
        state_change_timestamp: p.state_change_timestamp()?,
        status_errno: p.status_errno()?,
        tasks_current: p.tasks_current()?,
        timeout_clean_usec: p.timeout_clean_usec()?,
        watchdog_usec: p.watchdog_usec()?,
    })
}

fn parse_unit(
    stats: &mut SystemdUnitStats,
    unit: (
        String,              // The primary unit name as string
        String,              // The human readable description string
        String, // The load state (i.e. whether the unit file has been loaded successfully)
        String, // The active state (i.e. whether the unit is currently started or not)
        String, // The sub state (i.e. unit type more specific state)
        String, // A unit that is being followed in its state by this unit, if there is any, otherwise the empty string
        dbus::Path<'static>, // The unit object path
        u32,    // If there is a job queued for the job unit, the numeric job id, 0 otherwise
        String, // The job type as string
        dbus::Path<'static>, // The job object path
    ),
) {
    // Count unit type
    match unit.0.split('.').collect::<Vec<&str>>()[1] {
        "automount" => stats.automount_units += 1,
        "device" => stats.device_units += 1,
        "mount" => stats.mount_units += 1,
        "path" => stats.path_units += 1,
        "scope" => stats.scope_units += 1,
        "service" => stats.service_units += 1,
        "slice" => stats.slice_units += 1,
        "socket" => stats.socket_units += 1,
        "target" => stats.target_units += 1,
        "timer" => stats.timer_units += 1,
        unknown => debug!("Found unhandled '{}' unit type", unknown),
    };
    // Count load state
    match unit.2.as_str() {
        "loaded" => stats.loaded_units += 1,
        "masked" => stats.masked_units += 1,
        "not-found" => stats.not_found_units += 1,
        _ => debug!("{} is not loaded. It's {}", unit.0, unit.2),
    };
    // Count unit status
    match unit.3.as_str() {
        "active" => stats.active_units += 1,
        "failed" => stats.failed_units += 1,
        "inactive" => stats.inactive_units += 1,
        unknown => debug!("Found unhandled '{}' unit state", unknown),
    };
    // Count jobs queued
    if unit.7 != 0 {
        stats.jobs_queued += 1;
    }
}

pub fn parse_unit_state(
    dbus_address: &str,
    services_to_get_stats: Vec<&String>,
) -> Result<SystemdUnitStats, Box<dyn std::error::Error + Send + Sync>> {
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", dbus_address);
    let mut stats = SystemdUnitStats::default();
    let c = Connection::new_system()?;
    let p = c.with_proxy(
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        Duration::new(5, 0),
    );
    use crate::systemd_dbus::OrgFreedesktopSystemd1Manager;
    let units = p.list_units()?;
    stats.total_units = units.len() as u64;
    for unit in units {
        parse_unit(&mut stats, unit.clone());
        if services_to_get_stats.contains(&&unit.0) {
            debug!("Collecting service stats for {}", &unit.0);
            match parse_service(&c, &unit.0, &unit.6) {
                Ok(service_stats) => {
                    stats.service_stats.insert(unit.0.clone(), service_stats);
                }
                Err(err) => error!(
                    "Unable to get service stats for {} {}: {:#?}",
                    &unit.0, &unit.6, err
                ),
            }
        }
    }
    debug!("unit stats: {:?}", stats);
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unit_parse() {
        let expected_stats = SystemdUnitStats {
            active_units: 0,
            automount_units: 0,
            device_units: 0,
            failed_units: 0,
            inactive_units: 1,
            jobs_queued: 0,
            loaded_units: 1,
            masked_units: 0,
            mount_units: 0,
            not_found_units: 0,
            path_units: 0,
            scope_units: 0,
            service_units: 0,
            slice_units: 0,
            socket_units: 0,
            target_units: 0,
            timer_units: 1,
            total_units: 0,
            service_stats: HashMap::new(),
        };
        let mut stats = SystemdUnitStats::default();
        let systemd_unit = (
            String::from("apport-autoreport.timer"),
            String::from("Process error reports when automatic reporting is enabled (timer based)"),
            String::from("loaded"),
            String::from("inactive"),
            String::from("dead"),
            String::from(""),
            dbus::Path::new("/org/freedesktop/systemd1/unit/apport_2dautoreport_2etimer\0")
                .unwrap(),
            0 as u32,
            String::from(""),
            dbus::Path::new("/\0").unwrap(),
        );
        parse_unit(&mut stats, systemd_unit);
        assert_eq!(expected_stats, stats);
    }
}
