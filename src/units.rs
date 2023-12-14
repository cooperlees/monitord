//! # units module
//!
//! All main systemd unit statistics. Counts of types of units, unit states and
//! queued jobs. We also house service specific statistics and system unit states.

use std::collections::HashMap;
use std::convert::TryInto;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Result;
use dbus::blocking::Connection;
use indexmap::map::IndexMap;
use int_enum::IntEnum;
use serde_repr::*;
use struct_field_names_as_array::FieldNamesAsArray;
use strum_macros::EnumString;
use tracing::debug;
use tracing::error;

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, FieldNamesAsArray, PartialEq,
)]

/// Struct with all the unit count statistics
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
    pub unit_states: HashMap<String, UnitStates>,
}

/// Selected subset of metrics collected from systemd OrgFreedesktopSystemd1Service
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, FieldNamesAsArray, PartialEq,
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

/// Collection of a Unit active and load state: https://www.freedesktop.org/software/systemd/man/org.freedesktop.systemd1.html
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, FieldNamesAsArray, PartialEq,
)]
pub struct UnitStates {
    pub active_state: SystemdUnitActiveState,
    pub load_state: SystemdUnitLoadState,
    pub unhealthy: bool,
}

// Declare state types
// Reference: https://www.freedesktop.org/software/systemd/man/org.freedesktop.systemd1.html
// SubState can be unit-type-specific so can't enum

/// Possible systemd unit active states enumerated
#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr,
    Deserialize_repr,
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    EnumString,
    IntEnum,
    strum_macros::ToString,
)]
#[repr(u8)]
pub enum SystemdUnitActiveState {
    #[default]
    unknown = 0,
    active = 1,
    reloading = 2,
    inactive = 3,
    failed = 4,
    activating = 5,
    deactivating = 6,
}

/// Possible systemd unit load states enumerated
#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr,
    Deserialize_repr,
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    EnumString,
    IntEnum,
    strum_macros::ToString,
)]
#[repr(u8)]
pub enum SystemdUnitLoadState {
    #[default]
    unknown = 0,
    loaded = 1,
    error = 2,
    masked = 3,
}

pub const SERVICE_FIELD_NAMES: &[&str] = ServiceStats::FIELD_NAMES_AS_ARRAY;
pub const UNIT_FIELD_NAMES: &[&str] = SystemdUnitStats::FIELD_NAMES_AS_ARRAY;
pub const UNIT_STATES_FIELD_NAMES: &[&str] = UnitStates::FIELD_NAMES_AS_ARRAY;

/// Pull out selected systemd service statistics
fn parse_service(c: &Connection, name: &str, path: &str) -> Result<ServiceStats, dbus::Error> {
    debug!("Parsing service {} stats", name);
    let p = c.with_proxy("org.freedesktop.systemd1", path, Duration::new(2, 0));
    use crate::dbus::units::OrgFreedesktopSystemd1Service;
    use crate::dbus::units::OrgFreedesktopSystemd1Unit;

    let processes = match p.get_processes()?.len().try_into() {
        Ok(procs) => procs,
        Err(err) => {
            error!(
                "Unable to get process count for {} into u32: {:?}",
                name, err
            );
            0
        }
    };

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
        processes,
        restart_usec: p.restart_usec()?,
        state_change_timestamp: p.state_change_timestamp()?,
        status_errno: p.status_errno()?,
        tasks_current: p.tasks_current()?,
        timeout_clean_usec: p.timeout_clean_usec()?,
        watchdog_usec: p.watchdog_usec()?,
    })
}

/// Parse state of a unit into our unit_states hash
pub fn parse_state(
    stats: &mut SystemdUnitStats,
    unit: (
        String, // unit name
        String,
        String, // load state
        String, // active state
        String,
        String,
        dbus::Path<'static>,
        u32,
        String,
        dbus::Path<'static>,
    ),
    allowlist: &[&String],
    blocklist: &[&String],
) {
    let unit_name = unit.0;
    if blocklist.contains(&&unit_name) {
        debug!("Skipping state stats for {} due to blocklist", unit_name);
        return;
    }
    if !allowlist.is_empty() && !allowlist.contains(&&unit_name) {
        debug!(
            "Skipping state stats for {} due to not being in allowlist",
            unit_name
        );
        return;
    }
    let active_state =
        SystemdUnitActiveState::from_str(&unit.3).unwrap_or(SystemdUnitActiveState::unknown);
    stats.unit_states.insert(
        unit_name.clone(),
        UnitStates {
            active_state,
            load_state: SystemdUnitLoadState::from_str(&unit.2)
                .unwrap_or(SystemdUnitLoadState::unknown),
            unhealthy: !matches!(active_state, SystemdUnitActiveState::active),
        },
    );
}

/// Parse a unit and add to overall counts of state, type etc.
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

/// Pull all units from dbus and count how system is setup and behaving
pub fn parse_unit_state(
    dbus_address: &str,
    config_map: IndexMap<String, IndexMap<String, Option<String>>>,
) -> Result<SystemdUnitStats, Box<dyn std::error::Error + Send + Sync>> {
    // Parse out some config
    let services_to_get_stats: Vec<&String> = match config_map.get("services") {
        Some(services_hash) => services_hash.keys().collect(),
        None => Vec::from([]),
    };
    let state_stats_allowlist: Vec<&String> = match config_map.get("units.state_stats.allowlist") {
        Some(services_hash) => services_hash.keys().collect(),
        None => Vec::from([]),
    };
    if !state_stats_allowlist.is_empty() {
        debug!("Using unit state allowlist: {:?}", state_stats_allowlist);
    }
    let state_stats_blocklist: Vec<&String> = match config_map.get("units.state_stats.blocklist") {
        Some(services_hash) => services_hash.keys().collect(),
        None => Vec::from([]),
    };
    if !state_stats_allowlist.is_empty() {
        debug!("Using unit state blocklist: {:?}", state_stats_allowlist);
    }

    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", dbus_address);
    let mut stats = SystemdUnitStats::default();
    let c = Connection::new_system()?;
    let p = c.with_proxy(
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        Duration::new(5, 0),
    );
    use crate::dbus::systemd::OrgFreedesktopSystemd1Manager;
    let units = p.list_units()?;
    stats.total_units = units.len() as u64;
    for unit in units {
        // Collect unit types + states counts
        parse_unit(&mut stats, unit.clone());

        // Collect per unit state stats - ActiveState + LoadState
        // Not collecting SubState (yet)
        if config_map["units"].contains_key("state_stats")
            && config_map["units"]["state_stats"] == Some(String::from("true"))
        {
            parse_state(
                &mut stats,
                unit.clone(),
                &state_stats_allowlist,
                &state_stats_blocklist,
            );
        }

        // Collect service stats
        if services_to_get_stats.contains(&&unit.0) {
            debug!("Collecting service stats for {:?}", &unit);
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

    fn get_unit_file() -> (
        String, // unit name
        String,
        String, // load state
        String, // active state
        String,
        String,
        dbus::Path<'static>,
        u32,
        String,
        dbus::Path<'static>,
    ) {
        (
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
        )
    }

    #[test]
    fn test_state_parse() {
        let test_unit_name = String::from("apport-autoreport.timer");
        let expected_stats = SystemdUnitStats {
            active_units: 0,
            automount_units: 0,
            device_units: 0,
            failed_units: 0,
            inactive_units: 0,
            jobs_queued: 0,
            loaded_units: 0,
            masked_units: 0,
            mount_units: 0,
            not_found_units: 0,
            path_units: 0,
            scope_units: 0,
            service_units: 0,
            slice_units: 0,
            socket_units: 0,
            target_units: 0,
            timer_units: 0,
            total_units: 0,
            service_stats: HashMap::new(),
            unit_states: HashMap::from([(
                test_unit_name.clone(),
                UnitStates {
                    active_state: SystemdUnitActiveState::inactive,
                    load_state: SystemdUnitLoadState::loaded,
                    unhealthy: true,
                },
            )]),
        };
        let mut stats = SystemdUnitStats::default();
        let systemd_unit = get_unit_file();

        // Test no allow list or blocklist
        parse_state(&mut stats, systemd_unit.clone(), &vec![], &vec![]);
        assert_eq!(expected_stats, stats);

        // Create some allow/block lists
        let allowlist = Vec::from([&test_unit_name]);
        let blocklist = Vec::from([&test_unit_name]);

        // test no blocklist and only allow list - Should equal the same as no lists above
        let mut allowlist_stats = SystemdUnitStats::default();
        parse_state(
            &mut allowlist_stats,
            systemd_unit.clone(),
            &allowlist,
            &vec![],
        );
        assert_eq!(expected_stats, allowlist_stats);

        // test blocklist with allow list (show it's preferred)
        let mut blocklist_stats = SystemdUnitStats::default();
        let expected_blocklist_stats = SystemdUnitStats::default();
        parse_state(&mut blocklist_stats, systemd_unit, &allowlist, &blocklist);
        assert_eq!(expected_blocklist_stats, blocklist_stats);
    }

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
            unit_states: HashMap::new(),
        };
        let mut stats = SystemdUnitStats::default();
        let systemd_unit = get_unit_file();
        parse_unit(&mut stats, systemd_unit);
        assert_eq!(expected_stats, stats);
    }
}
