//! # monitord Crate
//!
//! `monitord` is a library to gather statistics about systemd.

use std::sync::Arc;

use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use tokio::sync::RwLock;
use tracing::error;
use tracing::info;
use tracing::warn;

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
pub mod units;

pub const DEFAULT_DBUS_ADDRESS: &str = "unix:path=/run/dbus/system_bus_socket";

/// Stats collected for a single systemd-nspawn container or VM managed by systemd-machined
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
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
}

/// Root struct containing all enabled monitord metrics for the host system and containers
#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Eq, PartialEq)]
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

/// Main statictic collection function running what's required by configuration in parallel
/// Takes an optional locked stats struct to update and to output stats to STDOUT or not
pub async fn stat_collector(
    config: config::Config,
    maybe_locked_stats: Option<Arc<RwLock<MonitordStats>>>,
    output_stats: bool,
) -> anyhow::Result<()> {
    let mut collect_interval_ms: u128 = 0;
    if config.monitord.daemon {
        collect_interval_ms = (config.monitord.daemon_stats_refresh_secs * 1000).into();
    }

    let config = Arc::new(config);
    let locked_monitord_stats: Arc<RwLock<MonitordStats>> =
        maybe_locked_stats.unwrap_or(Arc::new(RwLock::new(MonitordStats::default())));
    let locked_machine_stats: Arc<RwLock<MachineStats>> =
        Arc::new(RwLock::new(MachineStats::default()));
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &config.monitord.dbus_address);
    let sdc = zbus::connection::Builder::system()?
        .method_timeout(std::time::Duration::from_secs(config.monitord.dbus_timeout))
        .build()
        .await?;
    let mut join_set = tokio::task::JoinSet::new();

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
            join_set.spawn(crate::networkd::update_networkd_stats(
                config.networkd.link_state_dir.clone(),
                None,
                sdc.clone(),
                locked_machine_stats.clone(),
            ));
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
            join_set.spawn(crate::units::update_unit_stats(
                Arc::clone(&config),
                sdc.clone(),
                locked_machine_stats.clone(),
            ));
        }

        if config.machines.enabled {
            join_set.spawn(crate::machines::update_machines_stats(
                Arc::clone(&config),
                sdc.clone(),
                locked_monitord_stats.clone(),
            ));
        }

        if config.dbus_stats.enabled {
            join_set.spawn(crate::dbus_stats::update_dbus_stats(
                Arc::clone(&config),
                sdc.clone(),
                locked_machine_stats.clone(),
            ));
        }

        if join_set.len() == 1 {
            warn!("No collectors except systemd version scheduled to run. Exiting");
        }

        // Check all collection for errors and log if one fails
        while let Some(res) = join_set.join_next().await {
            match res {
                Ok(r) => match r {
                    Ok(_) => (),
                    Err(e) => {
                        error!("Collection specific failure: {:?}", e);
                    }
                },
                Err(e) => {
                    error!("Join error: {:?}", e);
                }
            }
        }

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
        }

        let elapsed_runtime_ms = collect_start_time.elapsed().as_millis();

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
    Ok(())
}
