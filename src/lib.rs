//! # monitord Crate
//!
//! `monitord` is a library to gather statistics about systemd.

use std::sync::Arc;

use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use tokio::sync::RwLock;
use tokio::time::timeout;
use tracing::error;
use tracing::info;
use tracing::warn;

pub mod config;
pub(crate) mod dbus;
pub mod json;
pub mod logging;
pub mod machines;
pub mod networkd;
pub mod pid1;
pub mod system;
pub mod timer;
pub mod units;

pub const DEFAULT_DBUS_ADDRESS: &str = "unix:path=/run/dbus/system_bus_socket";

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct MachineStats {
    pub networkd: networkd::NetworkdState,
    pub pid1: Option<pid1::Pid1Stats>,
    pub system_state: system::SystemdSystemState,
    pub units: units::SystemdUnitStats,
    pub version: system::SystemdVersion,
}

/// Main monitord stats struct collection all enabled stats
#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Eq, PartialEq)]
pub struct MonitordStats {
    pub networkd: networkd::NetworkdState,
    pub pid1: Option<pid1::Pid1Stats>,
    pub system_state: system::SystemdSystemState,
    pub units: units::SystemdUnitStats,
    pub version: system::SystemdVersion,
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
            json::flatten(stats, &key_prefix.to_string()).expect("Invalid JSON serialization")
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

    let locked_monitord_stats: Arc<RwLock<MonitordStats>> =
        maybe_locked_stats.unwrap_or(Arc::new(RwLock::new(MonitordStats::default())));
    let locked_machine_stats: Arc<RwLock<MachineStats>> =
        Arc::new(RwLock::new(MachineStats::default()));
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &config.monitord.dbus_address);
    let sdc = zbus::Connection::system().await?;
    let mut join_set = tokio::task::JoinSet::new();

    loop {
        let collect_start_time = Instant::now();
        info!("Starting stat collection run");

        // Always collect systemd version
        let sdc_clone = sdc.clone();
        let stats_clone = locked_machine_stats.clone();
        join_set.spawn(async move {
            match timeout(
                Duration::from_secs(config.monitord.dbus_timeout),
                crate::system::update_version(sdc_clone, stats_clone),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => Err(anyhow::anyhow!("Timeout while collecting systemd version")),
            }
        });

        // Collect pid1 procfs stats
        if config.pid1.enabled {
            let stats_clone = locked_machine_stats.clone();
            join_set.spawn(async move {
                match timeout(
                    Duration::from_secs(config.monitord.dbus_timeout),
                    crate::pid1::update_pid1_stats(1, stats_clone),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => Err(anyhow::anyhow!("Timeout while collecting pid1 stats")),
                }
            });
        }

        // Run networkd collector if enabled
        if config.networkd.enabled {
            let sdc_clone = sdc.clone();
            let stats_clone = locked_machine_stats.clone();
            let config_clone = config.clone();
            join_set.spawn(async move {
                match timeout(
                    Duration::from_secs(config_clone.monitord.dbus_timeout),
                    crate::networkd::update_networkd_stats(
                        config_clone.networkd.link_state_dir.clone(),
                        None,
                        sdc_clone,
                        stats_clone,
                    ),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => Err(anyhow::anyhow!("Timeout while collecting networkd stats")),
                }
            });
        }

        // Run system running (SystemState) state collector
        if config.system_state.enabled {
            let sdc_clone = sdc.clone();
            let stats_clone = locked_machine_stats.clone();
            join_set.spawn(async move {
                match timeout(
                    Duration::from_secs(config.monitord.dbus_timeout),
                    crate::system::update_system_stats(sdc_clone, stats_clone),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => Err(anyhow::anyhow!(
                        "Timeout while collecting system state stats"
                    )),
                }
            });
        }

        // Run service collectors if there are services listed in config
        if config.units.enabled {
            let sdc_clone = sdc.clone();
            let stats_clone = locked_machine_stats.clone();
            let config_clone = config.clone();
            join_set.spawn(async move {
                match timeout(
                    Duration::from_secs(config.monitord.dbus_timeout),
                    crate::units::update_unit_stats(config_clone, sdc_clone, stats_clone),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => Err(anyhow::anyhow!("Timeout while collecting unit stats")),
                }
            });
        }

        if config.machines.enabled {
            let sdc_clone = sdc.clone();
            let stats_clone = locked_monitord_stats.clone();
            let config_clone = config.clone();
            join_set.spawn(async move {
                match timeout(
                    Duration::from_secs(config_clone.monitord.dbus_timeout),
                    crate::machines::update_machines_stats(config_clone, sdc_clone, stats_clone),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => Err(anyhow::anyhow!("Timeout while collecting machine stats")),
                }
            });
        }

        if join_set.len() == 1 {
            warn!("No collectors execpt systemd version scheduled to run. Exiting");
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
