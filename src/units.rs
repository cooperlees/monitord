//! # units module
//!
//! All main systemd unit statistics. Counts of types of units, unit states and
//! queued jobs. We also house service specific statistics and system unit states.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::Result;
use int_enum::IntEnum;
use serde_repr::*;
use struct_field_names_as_array::FieldNamesAsArray;
use strum_macros::EnumIter;
use strum_macros::EnumString;
use tokio::sync::RwLock;
use tracing::debug;
use tracing::error;
use zbus::zvariant::ObjectPath;
use zbus::zvariant::OwnedObjectPath;

use crate::MonitordStats;

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

/// Collection of a Unit active and load state: <https://www.freedesktop.org/software/systemd/man/org.freedesktop.systemd1.html>
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, FieldNamesAsArray, PartialEq,
)]
pub struct UnitStates {
    pub active_state: SystemdUnitActiveState,
    pub load_state: SystemdUnitLoadState,
    // Unhealthy is only calculated for SystemdUnitLoadState::loaded units based on !SystemdActiveState::active
    // and !SystemdUnitLoadState::masked
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
    EnumIter,
    EnumString,
    IntEnum,
    strum_macros::Display,
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
    EnumIter,
    EnumString,
    IntEnum,
    strum_macros::Display,
)]
#[repr(u8)]
pub enum SystemdUnitLoadState {
    #[default]
    unknown = 0,
    loaded = 1,
    error = 2,
    masked = 3,
    not_found = 4,
}

pub const SERVICE_FIELD_NAMES: &[&str] = &ServiceStats::FIELD_NAMES_AS_ARRAY;
pub const UNIT_FIELD_NAMES: &[&str] = &SystemdUnitStats::FIELD_NAMES_AS_ARRAY;
pub const UNIT_STATES_FIELD_NAMES: &[&str] = &UnitStates::FIELD_NAMES_AS_ARRAY;

/// Pull out selected systemd service statistics
async fn parse_service(
    connection: &zbus::Connection,
    name: &str,
    path: &str,
) -> Result<ServiceStats, zbus::Error> {
    debug!("Parsing service {} stats", name);

    let sp = crate::dbus::zbus_service::ServiceProxy::builder(connection)
        .path(ObjectPath::try_from(path)?)?
        .build()
        .await?;
    let up = crate::dbus::zbus_unit::UnitProxy::builder(connection)
        .path(ObjectPath::try_from(path)?)?
        .build()
        .await?;

    let processes = match sp.get_processes().await?.len().try_into() {
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
        active_enter_timestamp: up.active_enter_timestamp().await?,
        active_exit_timestamp: up.active_exit_timestamp().await?,
        cpuusage_nsec: sp.cpuusage_nsec().await?,
        inactive_exit_timestamp: up.inactive_exit_timestamp().await?,
        ioread_bytes: sp.ioread_bytes().await?,
        ioread_operations: sp.ioread_operations().await?,
        memory_current: sp.memory_current().await?,
        memory_available: sp.memory_available().await?,
        nrestarts: sp.nrestarts().await?,
        processes,
        restart_usec: sp.restart_usec().await?,
        state_change_timestamp: up.state_change_timestamp().await?,
        status_errno: sp.status_errno().await?,
        tasks_current: sp.tasks_current().await?,
        timeout_clean_usec: sp.timeout_clean_usec().await?,
        watchdog_usec: sp.watchdog_usec().await?,
    })
}

/// Check if we're a loaded unit and if so evaluate if we're acitive or not
/// If we're not
/// Only potentially mark unhealthy for LOADED units that are not active
pub fn is_unit_unhealthy(
    active_state: SystemdUnitActiveState,
    load_state: SystemdUnitLoadState,
) -> bool {
    match load_state {
        // We're loaded so let's see if we're active or not
        SystemdUnitLoadState::loaded => !matches!(active_state, SystemdUnitActiveState::active),
        // An admin can change a unit to be masked on purpose
        // so we are going to ignore all masked units due to that
        SystemdUnitLoadState::masked => false,
        // Otherwise, we're unhealthy
        _ => true,
    }
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
        OwnedObjectPath,
        u32,
        String,
        OwnedObjectPath,
    ),
    allowlist: &[String],
    blocklist: &[String],
) {
    let unit_name = unit.0;
    if blocklist.contains(&unit_name) {
        debug!("Skipping state stats for {} due to blocklist", unit_name);
        return;
    }
    if !allowlist.is_empty() && !allowlist.contains(&unit_name) {
        return;
    }
    let active_state =
        SystemdUnitActiveState::from_str(&unit.3).unwrap_or(SystemdUnitActiveState::unknown);
    let load_state = SystemdUnitLoadState::from_str(&unit.2.replace('-', "_"))
        .unwrap_or(SystemdUnitLoadState::unknown);

    stats.unit_states.insert(
        unit_name.clone(),
        UnitStates {
            active_state,
            load_state,
            unhealthy: is_unit_unhealthy(active_state, load_state),
        },
    );
}

/// Parse a unit and add to overall counts of state, type etc.
fn parse_unit(
    stats: &mut SystemdUnitStats,
    unit: (
        String,          // The primary unit name as string
        String,          // The human readable description string
        String,          // The load state (i.e. whether the unit file has been loaded successfully)
        String,          // The active state (i.e. whether the unit is currently started or not)
        String,          // The sub state (i.e. unit type more specific state)
        String, // A unit that is being followed in its state by this unit, if there is any, otherwise the empty string
        OwnedObjectPath, // The unit object path
        u32,    // If there is a job queued for the job unit, the numeric job id, 0 otherwise
        String, // The job type as string
        OwnedObjectPath, // The job object path
    ),
) {
    // Count unit type
    match unit.0.rsplit('.').next() {
        Some("automount") => stats.automount_units += 1,
        Some("device") => stats.device_units += 1,
        Some("mount") => stats.mount_units += 1,
        Some("path") => stats.path_units += 1,
        Some("scope") => stats.scope_units += 1,
        Some("service") => stats.service_units += 1,
        Some("slice") => stats.slice_units += 1,
        Some("socket") => stats.socket_units += 1,
        Some("target") => stats.target_units += 1,
        Some("timer") => stats.timer_units += 1,
        unknown => debug!("Found unhandled '{:?}' unit type", unknown),
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
pub async fn parse_unit_state(
    config: &crate::config::Config,
    connection: &zbus::Connection,
) -> Result<SystemdUnitStats, Box<dyn std::error::Error + Send + Sync>> {
    if !config.units.state_stats_allowlist.is_empty() {
        debug!(
            "Using unit state allowlist: {:?}",
            config.units.state_stats_allowlist
        );
    }
    if !config.units.state_stats_blocklist.is_empty() {
        debug!(
            "Using unit state blocklist: {:?}",
            config.units.state_stats_blocklist,
        );
    }

    let mut stats = SystemdUnitStats::default();
    let p = crate::dbus::zbus_systemd::ManagerProxy::new(connection).await?;
    let units = p.list_units().await?;
    stats.total_units = units.len() as u64;
    for unit in units {
        // Collect unit types + states counts
        parse_unit(&mut stats, unit.clone());

        // Collect per unit state stats - ActiveState + LoadState
        // Not collecting SubState (yet)
        if config.units.state_stats {
            parse_state(
                &mut stats,
                unit.clone(),
                &config.units.state_stats_allowlist,
                &config.units.state_stats_blocklist,
            );
        }

        // Collect service stats
        if config.services.contains(&unit.0) {
            debug!("Collecting service stats for {:?}", &unit);
            match parse_service(connection, &unit.0, &unit.6).await {
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

/// Async wrapper than can update uni stats when passed a locked struct
pub async fn update_unit_stats(
    config: crate::config::Config,
    connection: zbus::Connection,
    locked_monitord_stats: Arc<RwLock<MonitordStats>>,
) -> anyhow::Result<()> {
    let mut monitord_stats = locked_monitord_stats.write().await;
    match parse_unit_state(&config, &connection).await {
        Ok(units_stats) => monitord_stats.units = units_stats,
        Err(err) => error!("units stats failed: {:?}", err),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    fn get_unit_file() -> (
        String, // unit name
        String,
        String, // load state
        String, // active state
        String,
        String,
        OwnedObjectPath,
        u32,
        String,
        OwnedObjectPath,
    ) {
        (
            String::from("apport-autoreport.timer"),
            String::from("Process error reports when automatic reporting is enabled (timer based)"),
            String::from("loaded"),
            String::from("inactive"),
            String::from("dead"),
            String::from(""),
            ObjectPath::try_from("/org/freedesktop/systemd1/unit/apport_2dautoreport_2etimer")
                .unwrap()
                .into(),
            0 as u32,
            String::from(""),
            ObjectPath::try_from("/").unwrap().into(),
        )
    }

    #[test]
    fn test_is_unit_healthy() {
        // Obvious active/loaded is healthy
        assert!(!is_unit_unhealthy(
            SystemdUnitActiveState::active,
            SystemdUnitLoadState::loaded
        ));
        // Not active + loaded is not healthy
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::activating,
            SystemdUnitLoadState::loaded
        ));
        // Not loaded + anything is just marked healthy as we're not expecting it to ever be healthy
        assert!(!is_unit_unhealthy(
            SystemdUnitActiveState::activating,
            SystemdUnitLoadState::masked
        ));
        // Make error + not_found unhealthy too
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::deactivating,
            SystemdUnitLoadState::not_found
        ));
        assert!(is_unit_unhealthy(
            // Can never really be active here with error, but check we ignore it
            SystemdUnitActiveState::active,
            SystemdUnitLoadState::error,
        ));
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
        let allowlist = Vec::from([test_unit_name.clone()]);
        let blocklist = Vec::from([test_unit_name]);

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

    #[test]
    fn test_iterators() {
        assert!(SystemdUnitActiveState::iter().collect::<Vec<_>>().len() > 0);
        assert!(SystemdUnitLoadState::iter().collect::<Vec<_>>().len() > 0);
    }
}
