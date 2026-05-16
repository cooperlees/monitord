//! # units module
//!
//! All main systemd unit statistics. Counts of types of units, unit states and
//! queued jobs. We also house service specific statistics and system unit states.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use struct_field_names_as_array::FieldNamesAsArray;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::debug;
use tracing::error;
use zbus::zvariant::ObjectPath;
use zbus::zvariant::OwnedObjectPath;

#[derive(Error, Debug)]
pub enum MonitordUnitsError {
    #[error("Units D-Bus error: {0}")]
    ZbusError(#[from] zbus::Error),
    #[error("Integer conversion error: {0}")]
    IntConversion(#[from] std::num::TryFromIntError),
    #[error("System time error: {0}")]
    SystemTimeError(#[from] std::time::SystemTimeError),
}

use crate::timer::TimerStats;
use crate::MachineStats;

// Re-export the enums and function from unit_constants for backwards compatibility
pub use crate::unit_constants::is_unit_unhealthy;
pub use crate::unit_constants::SystemdUnitActiveState;
pub use crate::unit_constants::SystemdUnitLoadState;

/// Inner timing breakdown for the units collector D-Bus phases.
///
/// Helps locate which step of unit collection dominates wall time when the
/// `units` collector is the slowest one in `MonitordStats::collector_timings`.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct UnitsCollectionTimings {
    /// Time for the systemd ListUnits D-Bus call (one batched call returning all units).
    pub list_units_ms: f64,
    /// Time for filesystem unit file stats collection (runs concurrently with list_units).
    pub unit_files_ms: f64,
    /// Time spent in the per-unit parse loop, including any per-unit D-Bus calls
    /// (timer property fetches, state stats, service stats).
    pub per_unit_loop_ms: f64,
    /// Number of timer units whose properties were fetched via D-Bus this run.
    pub timer_dbus_fetches: u64,
    /// Number of unit state D-Bus fetches this run (when state_stats_time_in_state is enabled).
    pub state_dbus_fetches: u64,
    /// Number of per-service D-Bus property fetches this run.
    pub service_dbus_fetches: u64,
}

/// Unit file counts for a scope (root or user), broken down by unit type.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct UnitFilesScope {
    /// Generated unit files by type (e.g. "service" => 2, "mount" => 5)
    pub generated: HashMap<String, u64>,
    /// Transient unit files by type (e.g. "service" => 10, "scope" => 6)
    pub transient: HashMap<String, u64>,
}

/// Unit file statistics collected from the filesystem for root and user scopes.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct UnitFilesStats {
    pub root: UnitFilesScope,
    pub user: UnitFilesScope,
}

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Default, FieldNamesAsArray, PartialEq,
)]

/// Aggregated systemd unit statistics: counts by type, load state, active state,
/// plus optional per-service and per-timer detailed metrics
pub struct SystemdUnitStats {
    /// Number of units in the "activating" state (in the process of being started)
    pub activating_units: u64,
    /// Number of units in the "active" state (currently started and running)
    pub active_units: u64,
    /// Number of automount units (on-demand filesystem mount points)
    pub automount_units: u64,
    /// Number of device units (kernel devices exposed to systemd by udev)
    pub device_units: u64,
    /// Number of units in the "failed" state (exited with error, crashed, or timed out)
    pub failed_units: u64,
    /// Number of units in the "inactive" state (not currently running)
    pub inactive_units: u64,
    /// Number of pending jobs queued in the systemd job scheduler
    pub jobs_queued: u64,
    /// Number of units whose unit file has been successfully loaded into memory
    pub loaded_units: u64,
    /// Number of units whose unit file is masked (symlinked to /dev/null, cannot be started)
    pub masked_units: u64,
    /// Number of mount units (filesystem mount points managed by systemd)
    pub mount_units: u64,
    /// Number of units whose unit file could not be found on disk
    pub not_found_units: u64,
    /// Number of path units (file/directory watch triggers)
    pub path_units: u64,
    /// Number of scope units (externally created process groups, e.g. user sessions)
    pub scope_units: u64,
    /// Number of service units (daemon/process lifecycle management)
    pub service_units: u64,
    /// Number of slice units (resource management groups in the cgroup hierarchy)
    pub slice_units: u64,
    /// Number of socket units (IPC/network socket activation endpoints)
    pub socket_units: u64,
    /// Number of target units (synchronization points for grouping units)
    pub target_units: u64,
    /// Number of timer units (calendar/monotonic scheduled triggers)
    pub timer_units: u64,
    /// Number of timer units with Persistent=yes (triggers missed runs after downtime)
    pub timer_persistent_units: u64,
    /// Number of timer units with RemainAfterElapse=yes (stays loaded after firing)
    pub timer_remain_after_elapse: u64,
    /// Total number of units known to systemd (all types, all states)
    pub total_units: u64,
    /// Unit file statistics from the filesystem (e.g. generator output counts)
    pub unit_files: UnitFilesStats,
    /// Per-service detailed metrics keyed by unit name (e.g. "sshd.service")
    pub service_stats: HashMap<String, ServiceStats>,
    /// Per-timer detailed metrics keyed by unit name (e.g. "logrotate.timer")
    pub timer_stats: HashMap<String, TimerStats>,
    /// Per-unit active/load state tracking keyed by unit name
    pub unit_states: HashMap<String, UnitStates>,
    /// Inner timing breakdown for this collector. Zero-valued before the first
    /// run completes or when the varlink path is taken.
    pub collection_timings: UnitsCollectionTimings,
}

/// Per-service metrics from the org.freedesktop.systemd1.Service and Unit D-Bus interfaces.
/// Ref: <https://www.freedesktop.org/software/systemd/man/org.freedesktop.systemd1.html>
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, FieldNamesAsArray, PartialEq,
)]
pub struct ServiceStats {
    /// Realtime timestamp (usec since epoch) when the unit most recently entered the active state
    pub active_enter_timestamp: u64,
    /// Realtime timestamp (usec since epoch) when the unit most recently left the active state
    pub active_exit_timestamp: u64,
    /// Total CPU time consumed by this service's cgroup in nanoseconds
    pub cpuusage_nsec: u64,
    /// Realtime timestamp (usec since epoch) when the unit most recently left the inactive state
    pub inactive_exit_timestamp: u64,
    /// Total bytes read from block I/O by this service's cgroup
    pub ioread_bytes: u64,
    /// Total number of block I/O read operations by this service's cgroup
    pub ioread_operations: u64,
    /// Memory available to the service (MemoryAvailable from cgroup), in bytes
    pub memory_available: u64,
    /// Current memory usage of the service's cgroup in bytes
    pub memory_current: u64,
    /// Number of times systemd has restarted this service (automatic restarts)
    pub nrestarts: u32,
    /// Current number of processes in this service's cgroup
    pub processes: u32,
    /// Configured restart delay for this service in microseconds (RestartUSec)
    pub restart_usec: u64,
    /// Realtime timestamp (usec since epoch) of the most recent state change of any kind
    pub state_change_timestamp: u64,
    /// errno-style exit status code from the main process (0 = success)
    pub status_errno: i32,
    /// Current number of tasks (threads) in this service's cgroup
    pub tasks_current: u64,
    /// Timeout in microseconds for the cleanup of resources after the service exits
    pub timeout_clean_usec: u64,
    /// Watchdog timeout in microseconds; the service must ping within this interval or be killed
    pub watchdog_usec: u64,
}

/// Per-unit state tracking combining active state, load state, and computed health.
/// Ref: <https://www.freedesktop.org/software/systemd/man/org.freedesktop.systemd1.html>
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, FieldNamesAsArray, PartialEq,
)]
pub struct UnitStates {
    /// Current active state of the unit (active, inactive, failed, activating, deactivating, reloading)
    pub active_state: SystemdUnitActiveState,
    /// Current load state of the unit (loaded, error, masked, not_found)
    pub load_state: SystemdUnitLoadState,
    /// Computed health flag: true when a loaded unit is not active, or when load state is error/not_found.
    /// Masked units are never marked unhealthy since masking is an intentional admin action.
    pub unhealthy: bool,
    /// Microseconds elapsed since the unit's most recent state change.
    /// None when time-in-state tracking is disabled in config (expensive D-Bus lookup per unit).
    pub time_in_state_usecs: Option<u64>,
}

// Declare state types
// Reference: https://www.freedesktop.org/software/systemd/man/org.freedesktop.systemd1.html
// SubState can be unit-type-specific so can't enum

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
) -> Result<ServiceStats, MonitordUnitsError> {
    debug!("Parsing service {} stats", name);

    let sp = crate::dbus::zbus_service::ServiceProxy::builder(connection)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .path(object_path.clone())?
        .build()
        .await?;
    let up = crate::dbus::zbus_unit::UnitProxy::builder(connection)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .path(object_path.clone())?
        .build()
        .await?;

    // Use tokio::join! without tokio::spawn to avoid per-task allocation overhead.
    // These all share the same D-Bus connection so spawn adds no parallelism benefit.
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
        up.active_enter_timestamp(),
        up.active_exit_timestamp(),
        sp.cpuusage_nsec(),
        up.inactive_exit_timestamp(),
        sp.ioread_bytes(),
        sp.ioread_operations(),
        sp.memory_current(),
        sp.memory_available(),
        sp.nrestarts(),
        sp.get_processes(),
        sp.restart_usec(),
        up.state_change_timestamp(),
        sp.status_errno(),
        sp.tasks_current(),
        sp.timeout_clean_usec(),
        sp.watchdog_usec(),
    );

    Ok(ServiceStats {
        active_enter_timestamp: active_enter_timestamp?,
        active_exit_timestamp: active_exit_timestamp?,
        cpuusage_nsec: cpuusage_nsec?,
        inactive_exit_timestamp: inactive_exit_timestamp?,
        ioread_bytes: ioread_bytes?,
        ioread_operations: ioread_operations?,
        memory_current: memory_current?,
        memory_available: memory_available?,
        nrestarts: nrestarts?,
        processes: processes?.len().try_into()?,
        restart_usec: restart_usec?,
        state_change_timestamp: state_change_timestamp?,
        status_errno: status_errno?,
        tasks_current: tasks_current?,
        timeout_clean_usec: timeout_clean_usec?,
        watchdog_usec: watchdog_usec?,
    })
}

async fn get_time_in_state(
    connection: Option<&zbus::Connection>,
    unit: &ListedUnit,
) -> Result<Option<u64>, MonitordUnitsError> {
    match connection {
        Some(c) => {
            let up = crate::dbus::zbus_unit::UnitProxy::builder(c)
                .cache_properties(zbus::proxy::CacheProperties::No)
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

/// Parse state of a unit into our unit_states hash.
///
/// Returns true when an actual time-in-state D-Bus fetch was performed,
/// so callers can keep `state_dbus_fetches` honest. Allowlist/blocklist
/// short-circuits and `state_stats_time_in_state = false` both return false.
pub async fn parse_state(
    stats: &mut SystemdUnitStats,
    unit: &ListedUnit,
    config: &crate::config::UnitsConfig,
    connection: Option<&zbus::Connection>,
) -> Result<bool, MonitordUnitsError> {
    if config.state_stats_blocklist.contains(&unit.name) {
        debug!("Skipping state stats for {} due to blocklist", &unit.name);
        return Ok(false);
    }
    if !config.state_stats_allowlist.is_empty()
        && !config.state_stats_allowlist.contains(&unit.name)
    {
        return Ok(false);
    }
    let active_state = SystemdUnitActiveState::from_str(&unit.active_state)
        .unwrap_or(SystemdUnitActiveState::unknown);
    let load_state = SystemdUnitLoadState::from_str(&unit.load_state.replace('-', "_"))
        .unwrap_or(SystemdUnitLoadState::unknown);

    // Get the state_change_timestamp to determine time in usecs we've been in current state
    let mut time_in_state_usecs: Option<u64> = None;
    let mut did_dbus_fetch = false;
    if config.state_stats_time_in_state {
        time_in_state_usecs = get_time_in_state(connection, unit).await?;
        // get_time_in_state only issues a D-Bus call when connection is Some;
        // the None path logs an error and returns Ok(None) without calling out.
        did_dbus_fetch = connection.is_some();
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
    Ok(did_dbus_fetch)
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
        "activating" => stats.activating_units += 1,
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

const TRANSIENT_DIR: &str = "/run/systemd/transient";

async fn count_unit_files_by_type(path: &str) -> HashMap<String, u64> {
    let mut dir = match tokio::fs::read_dir(path).await {
        Ok(d) => d,
        Err(err) => {
            debug!("Unable to read {}: {:?}", path, err);
            return HashMap::new();
        }
    };
    let mut counts = HashMap::new();
    while let Ok(Some(entry)) = dir.next_entry().await {
        let file_type = match entry.file_type().await {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        let unit_type = name
            .to_str()
            .and_then(|n| n.rsplit('.').next())
            .unwrap_or("unknown");
        *counts.entry(unit_type.to_string()).or_insert(0) += 1;
    }
    counts
}

fn merge_counts(target: &mut HashMap<String, u64>, source: HashMap<String, u64>) {
    for (unit_type, count) in source {
        *target.entry(unit_type).or_insert(0) += count;
    }
}

/// Enumerate the per-user systemd transient directories under `{fs_root}/run/user`.
async fn enumerate_user_transient_dirs(fs_root: &str) -> Vec<String> {
    let user_dir = format!("{fs_root}/run/user");
    match tokio::fs::read_dir(&user_dir).await {
        Ok(mut entries) => {
            let mut dirs = Vec::new();
            while let Ok(Some(entry)) = entries.next_entry().await {
                dirs.push(format!("{}/systemd/transient", entry.path().display()));
            }
            dirs
        }
        Err(err) => {
            debug!("Unable to read {}: {:?}", user_dir, err);
            Vec::new()
        }
    }
}

/// Collect unit file statistics from the filesystem.
/// `fs_root` is prepended to all paths — empty string for the host,
/// `/proc/<pid>/root` for containers.
///
/// All directory reads are issued in parallel: the three generator directories,
/// the root transient directory, and user-dir enumeration run concurrently in a
/// first batch; per-user transient reads run concurrently in a second batch.
pub async fn collect_unit_files_stats(fs_root: &str) -> UnitFilesStats {
    // Pre-bind formatted paths to extend their lifetime across the join.
    let gen_path = format!("{fs_root}/run/systemd/generator");
    let gen_early_path = format!("{fs_root}/run/systemd/generator.early");
    let gen_late_path = format!("{fs_root}/run/systemd/generator.late");
    let transient_path = format!("{fs_root}{TRANSIENT_DIR}");

    // First batch: fixed paths + user dir enumeration all in parallel.
    let (gen, gen_early, gen_late, root_transient, user_dirs) = tokio::join!(
        count_unit_files_by_type(&gen_path),
        count_unit_files_by_type(&gen_early_path),
        count_unit_files_by_type(&gen_late_path),
        count_unit_files_by_type(&transient_path),
        enumerate_user_transient_dirs(fs_root),
    );

    let mut root_generated = HashMap::new();
    merge_counts(&mut root_generated, gen);
    merge_counts(&mut root_generated, gen_early);
    merge_counts(&mut root_generated, gen_late);

    // Second batch: read every user transient directory in parallel.
    let user_transient_counts =
        futures_util::future::join_all(user_dirs.iter().map(|d| count_unit_files_by_type(d))).await;

    let mut user_transient = HashMap::new();
    for counts in user_transient_counts {
        merge_counts(&mut user_transient, counts);
    }

    UnitFilesStats {
        root: UnitFilesScope {
            generated: root_generated,
            transient: root_transient,
        },
        user: UnitFilesScope {
            generated: HashMap::new(),
            transient: user_transient,
        },
    }
}

/// Pull all units from dbus and count how system is setup and behaving
pub async fn parse_unit_state(
    config: &crate::config::Config,
    connection: &zbus::Connection,
    fs_root: &str,
) -> Result<SystemdUnitStats, MonitordUnitsError> {
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

    let p = crate::dbus::zbus_systemd::ManagerProxy::builder(connection)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;

    // Run filesystem collection and D-Bus list_units in parallel, timing each independently.
    let (unit_files_result, units_result) = tokio::join!(
        async {
            let start = Instant::now();
            let files = if config.units.unit_files {
                collect_unit_files_stats(fs_root).await
            } else {
                UnitFilesStats::default()
            };
            (files, start.elapsed().as_secs_f64() * 1000.0)
        },
        async {
            let start = Instant::now();
            let units = p.list_units().await;
            (units, start.elapsed().as_secs_f64() * 1000.0)
        },
    );
    let (unit_files, unit_files_ms) = unit_files_result;
    let (units_result, list_units_ms) = units_result;
    stats.collection_timings.unit_files_ms = unit_files_ms;
    stats.collection_timings.list_units_ms = list_units_ms;
    stats.unit_files = unit_files;

    let units = units_result?;
    stats.total_units = units.len() as u64;

    let per_unit_loop_start = Instant::now();
    let mut state_dbus_fetches: u64 = 0;
    let mut service_dbus_fetches: u64 = 0;
    let mut timer_dbus_fetches: u64 = 0;

    for unit_raw in units {
        let unit: ListedUnit = unit_raw.into();
        // Collect unit types + states counts
        parse_unit(&mut stats, &unit);

        // Collect per unit state stats - ActiveState + LoadState
        // Not collecting SubState (yet)
        if config.units.state_stats {
            let did_dbus_fetch =
                parse_state(&mut stats, &unit, &config.units, Some(connection)).await?;
            if did_dbus_fetch {
                state_dbus_fetches += 1;
            }
        }

        // Collect service stats
        if config.services.contains(&unit.name) {
            debug!("Collecting service stats for {:?}", &unit);
            match parse_service(connection, &unit.name, &unit.unit_object_path).await {
                Ok(service_stats) => {
                    stats.service_stats.insert(unit.name.clone(), service_stats);
                    service_dbus_fetches += 1;
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
                    Ok(ts) => {
                        timer_dbus_fetches += 1;
                        Some(ts)
                    }
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
    let per_unit_loop_elapsed = per_unit_loop_start.elapsed();
    stats.collection_timings.per_unit_loop_ms = per_unit_loop_elapsed.as_secs_f64() * 1000.0;
    stats.collection_timings.state_dbus_fetches = state_dbus_fetches;
    stats.collection_timings.service_dbus_fetches = service_dbus_fetches;
    stats.collection_timings.timer_dbus_fetches = timer_dbus_fetches;

    debug!("unit stats: {:?}", stats);
    Ok(stats)
}

/// Async wrapper that can update unit stats when passed a locked struct.
/// `fs_root` is prepended to filesystem paths for unit file stats —
/// empty string for the host, `/proc/<pid>/root` for containers.
pub async fn update_unit_stats(
    config: Arc<crate::config::Config>,
    connection: zbus::Connection,
    locked_machine_stats: Arc<RwLock<MachineStats>>,
    fs_root: String,
) -> anyhow::Result<()> {
    let mut machine_stats = locked_machine_stats.write().await;
    match parse_unit_state(&config, &connection, &fs_root).await {
        Ok(units_stats) => machine_stats.units = units_stats,
        Err(err) => error!("units stats failed: {:?}", err),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
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

    #[tokio::test]
    async fn test_state_parse() -> Result<(), MonitordUnitsError> {
        let test_unit_name = String::from("apport-autoreport.timer");
        let expected_stats = SystemdUnitStats {
            activating_units: 0,
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
            unit_files: UnitFilesStats::default(),
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
            collection_timings: UnitsCollectionTimings::default(),
        };
        let mut stats = SystemdUnitStats::default();
        let systemd_unit = get_unit_file();
        let mut config = crate::config::UnitsConfig::default();

        // Test no allow list or blocklist; with connection: None, parse_state
        // takes the no-op path inside get_time_in_state and returns false.
        let did_fetch = parse_state(&mut stats, &systemd_unit, &config, None).await?;
        assert_eq!(expected_stats, stats);
        assert!(!did_fetch);

        // Create an allow list
        config.state_stats_allowlist = HashSet::from([test_unit_name.clone()]);

        // test no blocklist and only allow list - Should equal the same as no lists above
        let mut allowlist_stats = SystemdUnitStats::default();
        let did_fetch = parse_state(&mut allowlist_stats, &systemd_unit, &config, None).await?;
        assert_eq!(expected_stats, allowlist_stats);
        assert!(!did_fetch);

        // Now add a blocklist
        config.state_stats_blocklist = HashSet::from([test_unit_name]);

        // test blocklist with allow list (show it's preferred)
        let mut blocklist_stats = SystemdUnitStats::default();
        let expected_blocklist_stats = SystemdUnitStats::default();
        let did_fetch = parse_state(&mut blocklist_stats, &systemd_unit, &config, None).await?;
        assert_eq!(expected_blocklist_stats, blocklist_stats);
        // Blocklist short-circuit must NOT count as a D-Bus fetch.
        assert!(!did_fetch);
        Ok(())
    }

    #[test]
    fn test_unit_parse() {
        let expected_stats = SystemdUnitStats {
            activating_units: 0,
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
            unit_files: UnitFilesStats::default(),
            service_stats: HashMap::new(),
            timer_stats: HashMap::new(),
            unit_states: HashMap::new(),
            collection_timings: UnitsCollectionTimings::default(),
        };
        let mut stats = SystemdUnitStats::default();
        let systemd_unit = get_unit_file();
        parse_unit(&mut stats, &systemd_unit);
        assert_eq!(expected_stats, stats);
    }

    #[test]
    fn test_unit_parse_activating() {
        let mut activating_unit = get_unit_file();
        activating_unit.active_state = String::from("activating");
        let mut stats = SystemdUnitStats::default();
        parse_unit(&mut stats, &activating_unit);
        assert_eq!(stats.activating_units, 1);
        assert_eq!(stats.active_units, 0);
        assert_eq!(stats.inactive_units, 0);
    }

    #[test]
    fn test_iterators() {
        assert!(SystemdUnitActiveState::iter().collect::<Vec<_>>().len() > 0);
        assert!(SystemdUnitLoadState::iter().collect::<Vec<_>>().len() > 0);
    }

    #[tokio::test]
    async fn test_count_unit_files_by_type() {
        let dir = tempfile::tempdir().expect("Unable to create temp dir");
        let path = dir.path();

        std::fs::write(path.join("sshd.service"), "").unwrap();
        std::fs::write(path.join("nginx.service"), "").unwrap();
        std::fs::write(path.join("boot.mount"), "").unwrap();
        std::fs::write(path.join("swap.swap"), "").unwrap();
        std::fs::create_dir(path.join("multi-user.target.wants")).unwrap();

        let counts = count_unit_files_by_type(path.to_str().unwrap()).await;
        assert_eq!(counts.get("service"), Some(&2));
        assert_eq!(counts.get("mount"), Some(&1));
        assert_eq!(counts.get("swap"), Some(&1));
        assert_eq!(counts.get("wants"), None);
        assert_eq!(counts.len(), 3);
    }

    #[tokio::test]
    async fn test_count_unit_files_by_type_nonexistent_dir() {
        let counts = count_unit_files_by_type("/nonexistent/path").await;
        assert!(counts.is_empty());
    }

    #[tokio::test]
    async fn test_collect_unit_files_stats_with_fs_root() {
        let root = tempfile::tempdir().expect("Unable to create temp dir");
        let root_path = root.path();

        let gen_dir = root_path.join("run/systemd/generator");
        std::fs::create_dir_all(&gen_dir).unwrap();
        std::fs::write(gen_dir.join("boot.mount"), "").unwrap();
        std::fs::write(gen_dir.join("swap.swap"), "").unwrap();

        let transient_dir = root_path.join("run/systemd/transient");
        std::fs::create_dir_all(&transient_dir).unwrap();
        std::fs::write(transient_dir.join("run-thing.service"), "").unwrap();

        let user_transient = root_path.join("run/user/1000/systemd/transient");
        std::fs::create_dir_all(&user_transient).unwrap();
        std::fs::write(user_transient.join("app-code.scope"), "").unwrap();
        std::fs::write(user_transient.join("app-term.scope"), "").unwrap();

        let stats = collect_unit_files_stats(root_path.to_str().unwrap()).await;
        assert_eq!(stats.root.generated.get("mount"), Some(&1));
        assert_eq!(stats.root.generated.get("swap"), Some(&1));
        assert_eq!(stats.root.transient.get("service"), Some(&1));
        assert_eq!(stats.user.transient.get("scope"), Some(&2));
        assert!(stats.user.generated.is_empty());
    }
}
