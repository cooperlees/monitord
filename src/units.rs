use std::time::Duration;

use anyhow::Result;
use dbus::blocking::Connection;
use log::debug;
use struct_field_names_as_array::FieldNamesAsArray;

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
}

pub const UNIT_FIELD_NAMES: &[&str] = SystemdUnitStats::FIELD_NAMES_AS_ARRAY;

fn parse_unit(
    stats: &mut SystemdUnitStats,
    unit: (
        String,
        String,
        String,
        String,
        String,
        String,
        dbus::Path<'static>,
        u32,
        String,
        dbus::Path<'static>,
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
        // TOOD: Change to debug logging ...
        unknown => debug!("Found unhandled '{}' unit state", unknown),
    };
    // Count jobs queued
    if unit.7 != 0 {
        stats.jobs_queued += 1;
    }
}

pub fn parse_unit_state() -> Result<SystemdUnitStats, Box<dyn std::error::Error>> {
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
        parse_unit(&mut stats, unit);
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
