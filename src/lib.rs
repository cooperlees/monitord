//! # monitord Crate
//!
//! `monitord` is a library to gather statistics about systemd.

use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use tokio::sync::RwLock;
use tracing::error;
use tracing::info;

pub mod config;
pub(crate) mod dbus;
pub mod json;
pub mod logging;
pub mod networkd;
pub mod pid1;
pub mod system;
pub mod units;

pub const DEFAULT_DBUS_ADDRESS: &str = "unix:path=/run/dbus/system_bus_socket";

/// Main monitord stats struct collection all enabled stats
#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Eq, PartialEq)]
pub struct MonitordStats {
    pub networkd: networkd::NetworkdState,
    pub pid1: Option<pid1::Pid1Stats>,
    pub system_state: system::SystemdSystemState,
    pub units: units::SystemdUnitStats,
    pub version: system::SystemdVersion,
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

/// Main statictic collection function running what's required by configuration
pub async fn stat_collector(config: config::Config) -> anyhow::Result<()> {
    let mut collect_interval_ms: u128 = 0;
    if config.monitord.daemon {
        collect_interval_ms = (config.monitord.daemon_stats_refresh_secs * 1000).into();
    }

    let locked_monitord_stats: Arc<RwLock<MonitordStats>> =
        Arc::new(RwLock::new(MonitordStats::default()));
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &config.monitord.dbus_address);
    let sdc = zbus::Connection::system().await?;
    let mut join_set = tokio::task::JoinSet::new();
    loop {
        let collect_start_time = Instant::now();
        let mut ran_collector_count: u8 = 0;

        info!("Starting stat collection run");

        // Collect pid1 procfs stats
        if config.pid1.enabled {
            ran_collector_count += 1;
            join_set.spawn(crate::pid1::update_pid1_stats(
                locked_monitord_stats.clone(),
            ));
        }

        // Run networkd collector if enabled
        if config.networkd.enabled {
            ran_collector_count += 1;
            join_set.spawn(crate::networkd::update_networkd_stats(
                config.networkd.link_state_dir.clone(),
                None,
                sdc.clone(),
                locked_monitord_stats.clone(),
            ));
        }

        // Run system running (SystemState) state collector
        if config.system_state.enabled {
            ran_collector_count += 1;
            join_set.spawn(crate::system::update_system_stats(
                sdc.clone(),
                locked_monitord_stats.clone(),
            ));
        }
        // Not incrementing the ran_collector_count on purpose as this is always on by default
        join_set.spawn(crate::system::update_version(
            sdc.clone(),
            locked_monitord_stats.clone(),
        ));

        // Run service collectors if there are services listed in config
        if config.units.enabled {
            ran_collector_count += 1;
            join_set.spawn(crate::units::update_unit_stats(
                config.clone(),
                sdc.clone(),
                locked_monitord_stats.clone(),
            ));
        }

        if ran_collector_count < 1 {
            error!("No collectors scheduled to run. Exiting");
            std::process::exit(1);
        }

        // Check all collection for errors
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

        let elapsed_runtime_ms = collect_start_time.elapsed().as_millis();
        info!("stat collection run took {}ms", elapsed_runtime_ms);
        {
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
        thread::sleep(Duration::from_millis(
            sleep_time_ms
                .try_into()
                .expect("Sleep time does not fit into a u64 :O"),
        ));
    }
    Ok(())
}
