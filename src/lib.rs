//! # monitord Crate
//!
//! `monitord` is a library to gather statistics about systemd.

use std::sync::Arc;

use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use thiserror::Error;
use tokio::sync::RwLock;
use tracing::error;
use tracing::info;
use tracing::warn;

#[derive(Error, Debug)]
pub enum MonitordError {
    #[error("D-Bus connection error: {0}")]
    ZbusError(#[from] zbus::Error),
}

pub mod boot;
pub mod config;
pub(crate) mod dbus;
pub mod dbus_stats;
pub mod json;
pub mod logging;
pub mod machines;
pub mod networkd;
pub mod pid1;
pub mod system;
pub mod timer;
pub mod unit_constants;
pub mod units;
pub mod varlink;
pub mod varlink_networkd;
pub mod varlink_units;
pub mod verify;

pub const DEFAULT_DBUS_ADDRESS: &str = "unix:path=/run/dbus/system_bus_socket";

/// Stats collected for a single systemd-nspawn container or VM managed by systemd-machined
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct MachineStats {
    /// systemd-networkd interface states inside the container
    pub networkd: networkd::NetworkdState,
    /// PID 1 process stats from procfs (using the container's leader PID)
    pub pid1: Option<pid1::Pid1Stats>,
    /// Overall systemd system state (e.g. running, degraded) inside the container
    pub system_state: system::SystemdSystemState,
    /// Aggregated systemd unit counts and per-service/timer stats inside the container
    pub units: units::SystemdUnitStats,
    /// systemd version running inside the container
    pub version: system::SystemdVersion,
    /// D-Bus daemon/broker statistics inside the container
    pub dbus_stats: Option<dbus_stats::DBusStats>,
    /// Boot blame statistics: slowest units at boot with activation times in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_blame: Option<boot::BootBlameStats>,
    /// Unit verification error statistics
    pub verify_stats: Option<verify::VerifyStats>,
}

/// Root struct containing all enabled monitord metrics for the host system and containers
#[derive(serde::Serialize, serde::Deserialize, Debug, Default, PartialEq)]
pub struct MonitordStats {
    /// systemd-networkd interface states and managed interface count
    pub networkd: networkd::NetworkdState,
    /// PID 1 (systemd) process stats from procfs: CPU, memory, FDs, tasks
    pub pid1: Option<pid1::Pid1Stats>,
    /// Overall systemd manager state (e.g. running, degraded, initializing)
    pub system_state: system::SystemdSystemState,
    /// Aggregated systemd unit counts by type/state and per-service/timer detailed metrics
    pub units: units::SystemdUnitStats,
    /// Installed systemd version (major.minor.revision.os)
    pub version: system::SystemdVersion,
    /// D-Bus daemon/broker statistics (connections, bus names, match rules, per-peer accounting)
    pub dbus_stats: Option<dbus_stats::DBusStats>,
    /// Per-container stats keyed by machine name, collected via systemd-machined
    pub machines: HashMap<String, MachineStats>,
    /// Boot blame statistics: slowest units at boot with activation times in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_blame: Option<boot::BootBlameStats>,
    /// Unit verification error statistics
    pub verify_stats: Option<verify::VerifyStats>,
    /// End-to-end duration of the last stat collection run in milliseconds.
    pub stat_collection_run_time_ms: f64,
}

/// Print statistics in the format set in configuration
pub fn print_stats(
    key_prefix: &str,
    output_format: &config::MonitordOutputFormat,
    stats: &MonitordStats,
) {
    match output_format {
        config::MonitordOutputFormat::Json => println!(
            "{}",
            serde_json::to_string(&stats).expect("Invalid JSON serialization")
        ),
        config::MonitordOutputFormat::JsonFlat => println!(
            "{}",
            json::flatten(stats, key_prefix).expect("Invalid JSON serialization")
        ),
        config::MonitordOutputFormat::JsonPretty => println!(
            "{}",
            serde_json::to_string_pretty(&stats).expect("Invalid JSON serialization")
        ),
    }
}

fn set_stat_collection_run_time(stats: &mut MonitordStats, elapsed_runtime: Duration) {
    stats.stat_collection_run_time_ms = elapsed_runtime.as_secs_f64() * 1000.0;
}

/// Reuse an existing D-Bus connection or create a new system bus connection.
async fn get_or_create_dbus_connection(
    config: &config::Config,
    maybe_connection: Option<zbus::Connection>,
) -> Result<zbus::Connection, MonitordError> {
    match maybe_connection {
        Some(conn) => Ok(conn),
        None => Ok(zbus::connection::Builder::system()?
            .method_timeout(std::time::Duration::from_secs(config.monitord.dbus_timeout))
            .build()
            .await?),
    }
}

/// Main statistic collection function running what's required by configuration in parallel
/// Takes an optional locked stats struct to update and to output stats to STDOUT or not.
/// Takes an optional D-Bus connection. Returns `Some(connection)` if the
/// collection cycle completed without errors (meaning the connection is reusable),
/// `None` if errors occurred.
pub async fn stat_collector(
    config: config::Config,
    maybe_locked_stats: Option<Arc<RwLock<MonitordStats>>>,
    output_stats: bool,
    maybe_connection: Option<zbus::Connection>,
) -> Result<Option<zbus::Connection>, MonitordError> {
    let mut collect_interval_ms: u128 = 0;
    if config.monitord.daemon {
        collect_interval_ms = (config.monitord.daemon_stats_refresh_secs * 1000).into();
    }

    let config = Arc::new(config);
    let locked_monitord_stats: Arc<RwLock<MonitordStats>> =
        maybe_locked_stats.unwrap_or(Arc::new(RwLock::new(MonitordStats::default())));
    let locked_machine_stats: Arc<RwLock<MachineStats>> =
        Arc::new(RwLock::new(MachineStats::default()));
    let cached_machine_connections: Arc<tokio::sync::Mutex<machines::MachineConnections>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &config.monitord.dbus_address);
    let sdc = get_or_create_dbus_connection(&config, maybe_connection).await?;
    let mut join_set = tokio::task::JoinSet::new();
    let mut had_error;

    loop {
        let collect_start_time = Instant::now();
        info!("Starting stat collection run");

        // Always collect systemd version

        join_set.spawn(crate::system::update_version(
            sdc.clone(),
            locked_machine_stats.clone(),
        ));

        // Collect pid1 procfs stats
        if config.pid1.enabled {
            join_set.spawn(crate::pid1::update_pid1_stats(
                1,
                locked_machine_stats.clone(),
            ));
        }

        // Run networkd collector if enabled
        if config.networkd.enabled {
            let config_clone = Arc::clone(&config);
            let sdc_clone = sdc.clone();
            let stats_clone = locked_machine_stats.clone();
            join_set.spawn(async move {
                if config_clone.varlink.enabled {
                    let socket_path = crate::varlink_networkd::NETWORK_SOCKET_PATH.to_string();
                    match crate::varlink_networkd::get_networkd_state(&socket_path).await {
                        Ok(networkd_stats) => {
                            let mut machine_stats = stats_clone.write().await;
                            machine_stats.networkd = networkd_stats;
                            return Ok(());
                        }
                        Err(err) => {
                            warn!(
                                "Varlink networkd stats failed, falling back to file-based: {:?}",
                                err
                            );
                        }
                    }
                }
                crate::networkd::update_networkd_stats(
                    config_clone.networkd.link_state_dir.clone(),
                    None,
                    sdc_clone,
                    stats_clone,
                )
                .await
            });
        }

        // Run system running (SystemState) state collector
        if config.system_state.enabled {
            join_set.spawn(crate::system::update_system_stats(
                sdc.clone(),
                locked_machine_stats.clone(),
            ));
        }

        // Run service collectors if there are services listed in config
        if config.units.enabled {
            let config_clone = Arc::clone(&config);
            let sdc_clone = sdc.clone();
            let stats_clone = locked_machine_stats.clone();
            join_set.spawn(async move {
                if config_clone.varlink.enabled {
                    let socket_path = crate::varlink_units::METRICS_SOCKET_PATH.to_string();
                    match crate::varlink_units::update_unit_stats(
                        Arc::clone(&config_clone),
                        stats_clone.clone(),
                        socket_path,
                    )
                    .await
                    {
                        Ok(()) => {
                            // Timer properties are not yet exposed via varlink; collect via D-Bus.
                            match crate::timer::collect_all_timers_dbus(&sdc_clone, &config_clone)
                                .await
                            {
                                Ok(timer_stats) => {
                                    let mut ms = stats_clone.write().await;
                                    ms.units.timer_stats = timer_stats.timer_stats;
                                    ms.units.timer_persistent_units =
                                        timer_stats.timer_persistent_units;
                                    ms.units.timer_remain_after_elapse =
                                        timer_stats.timer_remain_after_elapse;
                                }
                                Err(err) => {
                                    warn!("Varlink timer stats (D-Bus fallback) failed: {:?}", err);
                                }
                            }
                            return Ok(());
                        }
                        Err(err) => {
                            warn!(
                                "Varlink units stats failed, falling back to D-Bus: {:?}",
                                err
                            );
                        }
                    }
                }
                crate::units::update_unit_stats(config_clone, sdc_clone, stats_clone).await
            });
        }

        if config.machines.enabled {
            join_set.spawn(crate::machines::update_machines_stats(
                Arc::clone(&config),
                sdc.clone(),
                locked_monitord_stats.clone(),
                cached_machine_connections.clone(),
            ));
        }

        if config.dbus_stats.enabled {
            join_set.spawn(crate::dbus_stats::update_dbus_stats(
                Arc::clone(&config),
                sdc.clone(),
                locked_machine_stats.clone(),
            ));
        }

        if config.boot_blame.enabled {
            join_set.spawn(crate::boot::update_boot_blame_stats(
                Arc::clone(&config),
                sdc.clone(),
                locked_machine_stats.clone(),
            ));
        }

        if config.verify.enabled {
            join_set.spawn(crate::verify::update_verify_stats(
                sdc.clone(),
                locked_machine_stats.clone(),
                config.verify.allowlist.clone(),
                config.verify.blocklist.clone(),
            ));
        }

        if join_set.len() == 1 {
            warn!("No collectors except systemd version scheduled to run. Exiting");
        }

        // Check all collection for errors and log if one fails
        had_error = false;
        while let Some(res) = join_set.join_next().await {
            match res {
                Ok(r) => match r {
                    Ok(_) => (),
                    Err(e) => {
                        had_error = true;
                        error!("Collection specific failure: {:?}", e);
                    }
                },
                Err(e) => {
                    had_error = true;
                    error!("Join error: {:?}", e);
                }
            }
        }

        let elapsed_runtime = collect_start_time.elapsed();
        let elapsed_runtime_ms = elapsed_runtime.as_millis();

        {
            // Update monitord stats with machine stats
            let mut monitord_stats = locked_monitord_stats.write().await;
            let machine_stats = locked_machine_stats.read().await;
            monitord_stats.pid1 = machine_stats.pid1.clone();
            monitord_stats.networkd = machine_stats.networkd.clone();
            monitord_stats.system_state = machine_stats.system_state;
            monitord_stats.version = machine_stats.version.clone();
            monitord_stats.units = machine_stats.units.clone();
            monitord_stats.dbus_stats = machine_stats.dbus_stats.clone();
            monitord_stats.boot_blame = machine_stats.boot_blame.clone();
            monitord_stats.verify_stats = machine_stats.verify_stats.clone();
            set_stat_collection_run_time(&mut monitord_stats, elapsed_runtime);
        }

        info!("stat collection run took {}ms", elapsed_runtime_ms);
        if output_stats {
            let monitord_stats = locked_monitord_stats.read().await;
            print_stats(
                &config.monitord.key_prefix,
                &config.monitord.output_format,
                &monitord_stats,
            );
        }
        if !config.monitord.daemon {
            break;
        }
        let sleep_time_ms = collect_interval_ms - elapsed_runtime_ms;
        info!("stat collection sleeping for {}s 😴", sleep_time_ms / 1000);
        tokio::time::sleep(Duration::from_millis(
            sleep_time_ms
                .try_into()
                .expect("Sleep time does not fit into a u64 :O"),
        ))
        .await;
    }
    Ok(if had_error { None } else { Some(sdc) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stat_collection_run_time_ms_conversion() {
        let mut stats = MonitordStats::default();
        set_stat_collection_run_time(&mut stats, Duration::from_millis(5));
        assert_eq!(stats.stat_collection_run_time_ms, 5.0);

        set_stat_collection_run_time(&mut stats, Duration::from_micros(500));
        assert!((stats.stat_collection_run_time_ms - 0.5).abs() < f64::EPSILON);
    }
}
