//! # units module
//!
//! All main systemd unit statistics. Counts of types of units, unit states and
//! queued jobs. We also house service specific statistics and system unit states.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

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

use crate::timer::TimerStats;
use crate::MachineStats;

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
    pub timer_persistent_units: u64,
    pub timer_remain_after_elapse: u64,
    pub total_units: u64,
    pub service_stats: HashMap<String, ServiceStats>,
    pub timer_stats: HashMap<String, TimerStats>,
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
    // Time in microseconds since the unit state has changed ...
    // Expensive to lookup, so config disable available - Use optional to show that
    pub time_in_state_usecs: Option<u64>,
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

/// Representation of the returned Tuple from list_units - Better typing etc.
#[derive(Debug)]
pub struct ListedUnit {
    pub name: String,                      // The primary unit name
    pub description: String,               // The human readable description
    pub load_state: String, // The load state (i.e. whether the unit file has been loaded successfully)
    pub active_state: String, // The active state (i.e. whether the unit is currently started or not)
    pub sub_state: String,    // The sub state (i.e. unit type more specific state)
    pub follow_unit: String, // A unit that is being followed in its state by this unit, if there is any, otherwise the empty string
    pub unit_object_path: OwnedObjectPath, // The unit object path
    pub job_id: u32, // If there is a job queued for the job unit, the numeric job id, 0 otherwise
    pub job_type: String, // The job type as string
    pub job_object_path: OwnedObjectPath, // The job object path
}
impl
    From<(
        String,
        String,
        String,
        String,
        String,
        String,
        OwnedObjectPath,
        u32,
        String,
        OwnedObjectPath,
    )> for ListedUnit
{
    fn from(
        tuple: (
            String,
            String,
            String,
            String,
            String,
            String,
            OwnedObjectPath,
            u32,
            String,
            OwnedObjectPath,
        ),
    ) -> Self {
        ListedUnit {
            name: tuple.0,
            description: tuple.1,
            load_state: tuple.2,
            active_state: tuple.3,
            sub_state: tuple.4,
            follow_unit: tuple.5,
            unit_object_path: tuple.6,
            job_id: tuple.7,
            job_type: tuple.8,
            job_object_path: tuple.9,
        }
    }
}

pub const SERVICE_FIELD_NAMES: &[&str] = &ServiceStats::FIELD_NAMES_AS_ARRAY;
pub const UNIT_FIELD_NAMES: &[&str] = &SystemdUnitStats::FIELD_NAMES_AS_ARRAY;
pub const UNIT_STATES_FIELD_NAMES: &[&str] = &UnitStates::FIELD_NAMES_AS_ARRAY;

/// Pull out selected systemd service statistics
async fn parse_service(
    connection: &zbus::Connection,
    name: &str,
    object_path: &OwnedObjectPath,
) -> Result<ServiceStats> {
    debug!("Parsing service {} stats", name);

    let sp = Arc::new(
        crate::dbus::zbus_service::ServiceProxy::builder(connection)
            .path(object_path.clone())?
            .build()
            .await?,
    );
    let up = Arc::new(
        crate::dbus::zbus_unit::UnitProxy::builder(connection)
            .path(object_path.clone())?
            .build()
            .await?,
    );

    // TODO: Maybe introduce a semaphore to limit how many execute at once
    let (
        active_enter_timestamp,
        active_exit_timestamp,
        cpuusage_nsec,
        inactive_exit_timestamp,
        ioread_bytes,
        ioread_operations,
        memory_current,
        memory_available,
        nrestarts,
        processes,
        restart_usec,
        state_change_timestamp,
        status_errno,
        tasks_current,
        timeout_clean_usec,
        watchdog_usec,
    ) = tokio::join!(
        tokio::spawn({
            let spawn_up = up.clone();
            async move { spawn_up.active_enter_timestamp().await }
        }),
        tokio::spawn({
            let spawn_up = up.clone();
            async move { spawn_up.active_exit_timestamp().await }
        }),
        tokio::spawn({
            let spawn_sp = sp.clone();
            async move { spawn_sp.cpuusage_nsec().await }
        }),
        tokio::spawn({
            let spawn_up = up.clone();
            async move { spawn_up.inactive_exit_timestamp().await }
        }),
        tokio::spawn({
            let spawn_sp = sp.clone();
            async move { spawn_sp.ioread_bytes().await }
        }),
        tokio::spawn({
            let spawn_sp = sp.clone();
            async move { spawn_sp.ioread_operations().await }
        }),
        tokio::spawn({
            let spawn_sp = sp.clone();
            async move { spawn_sp.memory_current().await }
        }),
        tokio::spawn({
            let spawn_sp = sp.clone();
            async move { spawn_sp.memory_available().await }
        }),
        tokio::spawn({
            let spawn_sp = sp.clone();
            async move { spawn_sp.nrestarts().await }
        }),
        tokio::spawn({
            let spawn_sp = sp.clone();
            async move { spawn_sp.get_processes().await }
        }),
        tokio::spawn({
            let spawn_sp = sp.clone();
            async move { spawn_sp.restart_usec().await }
        }),
        tokio::spawn({
            let spawn_up = up.clone();
            async move { spawn_up.state_change_timestamp().await }
        }),
        tokio::spawn({
            let spawn_sp = sp.clone();
            async move { spawn_sp.status_errno().await }
        }),
        tokio::spawn({
            let spawn_sp = sp.clone();
            async move { spawn_sp.tasks_current().await }
        }),
        tokio::spawn({
            let spawn_sp = sp.clone();
            async move { spawn_sp.timeout_clean_usec().await }
        }),
        tokio::spawn({
            let spawn_sp = sp.clone();
            async move { spawn_sp.watchdog_usec().await }
        }),
    );

    Ok(ServiceStats {
        active_enter_timestamp: active_enter_timestamp??,
        active_exit_timestamp: active_exit_timestamp??,
        cpuusage_nsec: cpuusage_nsec??,
        inactive_exit_timestamp: inactive_exit_timestamp??,
        ioread_bytes: ioread_bytes??,
        ioread_operations: ioread_operations??,
        memory_current: memory_current??,
        memory_available: memory_available??,
        nrestarts: nrestarts??,
        processes: processes??[0].1,
        restart_usec: restart_usec??,
        state_change_timestamp: state_change_timestamp??,
        status_errno: status_errno??,
        tasks_current: tasks_current??,
        timeout_clean_usec: timeout_clean_usec??,
        watchdog_usec: watchdog_usec??,
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

async fn get_time_in_state(
    connection: Option<&zbus::Connection>,
    unit: &ListedUnit,
) -> Result<Option<u64>> {
    match connection {
        Some(c) => {
            let up = crate::dbus::zbus_unit::UnitProxy::builder(c)
                .path(ObjectPath::from(unit.unit_object_path.clone()))?
                .build()
                .await?;
            let now: u64 = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() * 1_000_000;
            let state_change_timestamp = match up.state_change_timestamp().await {
                Ok(sct) => sct,
                Err(err) => {
                    error!(
                        "Unable to get state_change_timestamp for {} - Setting to 0: {:?}",
                        &unit.name, err,
                    );
                    0
                }
            };
            Ok(Some(now - state_change_timestamp))
        }
        None => {
            error!("No zbus connection passed, but time_in_state_usecs enabled");
            Ok(None)
        }
    }
}

/// Parse state of a unit into our unit_states hash
pub async fn parse_state(
    stats: &mut SystemdUnitStats,
    unit: &ListedUnit,
    config: &crate::config::UnitsConfig,
    connection: Option<&zbus::Connection>,
) -> Result<()> {
    if config.state_stats_blocklist.contains(&unit.name) {
        debug!("Skipping state stats for {} due to blocklist", &unit.name);
        return Ok(());
    }
    if !config.state_stats_allowlist.is_empty()
        && !config.state_stats_allowlist.contains(&unit.name)
    {
        return Ok(());
    }
    let active_state = SystemdUnitActiveState::from_str(&unit.active_state)
        .unwrap_or(SystemdUnitActiveState::unknown);
    let load_state = SystemdUnitLoadState::from_str(&unit.load_state.replace('-', "_"))
        .unwrap_or(SystemdUnitLoadState::unknown);

    // Get the state_change_timestamp to determine time in usecs we've been in current state
    let mut time_in_state_usecs: Option<u64> = None;
    if config.state_stats_time_in_state {
        time_in_state_usecs = get_time_in_state(connection, unit).await?;
    }

    stats.unit_states.insert(
        unit.name.clone(),
        UnitStates {
            active_state,
            load_state,
            unhealthy: is_unit_unhealthy(active_state, load_state),
            time_in_state_usecs,
        },
    );
    Ok(())
}

/// Parse a unit and add to overall counts of state, type etc.
fn parse_unit(stats: &mut SystemdUnitStats, unit: &ListedUnit) {
    // Count unit type
    match unit.name.rsplit('.').next() {
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
    match unit.load_state.as_str() {
        "loaded" => stats.loaded_units += 1,
        "masked" => stats.masked_units += 1,
        "not-found" => stats.not_found_units += 1,
        _ => debug!("{} is not loaded. It's {}", unit.name, unit.load_state),
    };
    // Count unit status
    match unit.active_state.as_str() {
        "active" => stats.active_units += 1,
        "failed" => stats.failed_units += 1,
        "inactive" => stats.inactive_units += 1,
        unknown => debug!("Found unhandled '{}' unit state", unknown),
    };
    // Count jobs queued
    if unit.job_id != 0 {
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
    for unit_raw in units {
        let unit: ListedUnit = unit_raw.into();
        // Collect unit types + states counts
        parse_unit(&mut stats, &unit);

        // Collect per unit state stats - ActiveState + LoadState
        // Not collecting SubState (yet)
        if config.units.state_stats {
            parse_state(&mut stats, &unit, &config.units, Some(connection)).await?;
        }

        // Collect service stats
        if config.services.contains(&unit.name) {
            debug!("Collecting service stats for {:?}", &unit);
            match parse_service(connection, &unit.name, &unit.unit_object_path).await {
                Ok(service_stats) => {
                    stats.service_stats.insert(unit.name.clone(), service_stats);
                }
                Err(err) => error!(
                    "Unable to get service stats for {} {}: {:#?}",
                    &unit.name, &unit.unit_object_path, err
                ),
            }
        }

        // Collect timer stats
        if config.timers.enabled && unit.name.contains(".timer") {
            if config.timers.blocklist.contains(&unit.name) {
                debug!("Skipping timer stats for {} due to blocklist", &unit.name);
                continue;
            }
            if !config.timers.allowlist.is_empty() && !config.timers.allowlist.contains(&unit.name)
            {
                continue;
            }
            let timer_stats: Option<TimerStats> =
                match crate::timer::collect_timer_stats(connection, &mut stats, &unit).await {
                    Ok(ts) => Some(ts),
                    Err(err) => {
                        error!("Failed to get {} stats: {:#?}", &unit.name, err);
                        None
                    }
                };
            if let Some(ts) = timer_stats {
                stats.timer_stats.insert(unit.name.clone(), ts);
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
    locked_machine_stats: Arc<RwLock<MachineStats>>,
) -> anyhow::Result<()> {
    let mut machine_stats = locked_machine_stats.write().await;
    match parse_unit_state(&config, &connection).await {
        Ok(units_stats) => machine_stats.units = units_stats,
        Err(err) => error!("units stats failed: {:?}", err),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    fn get_unit_file() -> ListedUnit {
        ListedUnit {
            name: String::from("apport-autoreport.timer"),
            description: String::from(
                "Process error reports when automatic reporting is enabled (timer based)",
            ),
            load_state: String::from("loaded"),
            active_state: String::from("inactive"),
            sub_state: String::from("dead"),
            follow_unit: String::from(""),
            unit_object_path: ObjectPath::try_from(
                "/org/freedesktop/systemd1/unit/apport_2dautoreport_2etimer",
            )
            .expect("Unable to make an object path")
            .into(),
            job_id: 0,
            job_type: String::from(""),
            job_object_path: ObjectPath::try_from("/").unwrap().into(),
        }
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

    #[tokio::test]
    async fn test_state_parse() -> Result<()> {
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
            timer_persistent_units: 0,
            timer_remain_after_elapse: 0,
            total_units: 0,
            service_stats: HashMap::new(),
            timer_stats: HashMap::new(),
            unit_states: HashMap::from([(
                test_unit_name.clone(),
                UnitStates {
                    active_state: SystemdUnitActiveState::inactive,
                    load_state: SystemdUnitLoadState::loaded,
                    unhealthy: true,
                    time_in_state_usecs: None,
                },
            )]),
        };
        let mut stats = SystemdUnitStats::default();
        let systemd_unit = get_unit_file();
        let mut config = crate::config::UnitsConfig::default();

        // Test no allow list or blocklist
        parse_state(&mut stats, &systemd_unit, &config, None).await?;
        assert_eq!(expected_stats, stats);

        // Create an allow list
        config.state_stats_allowlist = Vec::from([test_unit_name.clone()]);

        // test no blocklist and only allow list - Should equal the same as no lists above
        let mut allowlist_stats = SystemdUnitStats::default();
        parse_state(&mut allowlist_stats, &systemd_unit, &config, None).await?;
        assert_eq!(expected_stats, allowlist_stats);

        // Now add a blocklist
        config.state_stats_blocklist = Vec::from([test_unit_name]);

        // test blocklist with allow list (show it's preferred)
        let mut blocklist_stats = SystemdUnitStats::default();
        let expected_blocklist_stats = SystemdUnitStats::default();
        parse_state(&mut blocklist_stats, &systemd_unit, &config, None).await?;
        assert_eq!(expected_blocklist_stats, blocklist_stats);
        Ok(())
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
            timer_persistent_units: 0,
            timer_remain_after_elapse: 0,
            total_units: 0,
            service_stats: HashMap::new(),
            timer_stats: HashMap::new(),
            unit_states: HashMap::new(),
        };
        let mut stats = SystemdUnitStats::default();
        let systemd_unit = get_unit_file();
        parse_unit(&mut stats, &systemd_unit);
        assert_eq!(expected_stats, stats);
    }

    #[test]
    fn test_iterators() {
        assert!(SystemdUnitActiveState::iter().collect::<Vec<_>>().len() > 0);
        assert!(SystemdUnitLoadState::iter().collect::<Vec<_>>().len() > 0);
    }
}
