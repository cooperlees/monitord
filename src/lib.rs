//! # monitord Crate
//!
//! `monitord` is a library to gather statistics about systemd.
//! Some APIs are a little ugly due to being a configparser INI based configuration
//! driven CLL at heart.

use std::thread;
use std::time::Duration;
use std::time::Instant;

use anyhow::Result;
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
pub async fn stat_collector(config: config::Config) -> Result<(), String> {
    let mut collect_interval_ms: u128 = 0;
    if config.monitord.daemon {
        collect_interval_ms = (config.monitord.daemon_stats_refresh_secs * 1000).into();
    }

    let mut monitord_stats = MonitordStats::default();
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &config.monitord.dbus_address);
    let sdc = match zbus::Connection::system().await {
        Ok(sdc) => sdc,
        Err(e) => {
            return Err(format!(
                "Unable to connect to system dbus via zbus: {:?}",
                e
            ))
        }
    };
    loop {
        let collect_start_time = Instant::now();
        let mut ran_collector_count: u8 = 0;

        info!("Starting stat collection run");

        // TODO: Refactor to run all async methods in parallel
        // Collect pid1 procfs stats
        if config.pid1.enabled {
            let pid1_stats = match tokio::task::spawn_blocking(crate::pid1::get_pid1_stats).await {
                Ok(p1s) => p1s,
                Err(err) => {
                    return Err(format!(
                        "Unable to spawn blocking around PID1 stats: {:?}",
                        err
                    ))
                }
            };
            monitord_stats.pid1 = match pid1_stats {
                Ok(s) => Some(s),
                Err(err) => {
                    error!("Unable to set pid1 stats: {:?}", err);
                    None
                }
            }
        }

        // Run networkd collector if enabled
        if config.networkd.enabled {
            ran_collector_count += 1;
            match networkd::parse_interface_state_files(&config.networkd.link_state_dir, None, &sdc)
                .await
            {
                Ok(networkd_stats) => monitord_stats.networkd = networkd_stats,
                Err(err) => error!("networkd stats failed: {:?}", err),
            }
        }

        // Run system running (SystemState) state collector
        if config.system_state.enabled {
            ran_collector_count += 1;
            monitord_stats.system_state = crate::system::get_system_state(&sdc)
                .await
                .map_err(|e| format!("Error getting system state: {:?}", e))?;
        }
        // Not incrementing the ran_collector_count on purpose as this is always on by default
        monitord_stats.version = crate::system::get_version(&sdc)
            .await
            .map_err(|e| format!("Error getting systemd versions: {:?}", e))?;

        // Run service collectors if there are services listed in config
        if config.units.enabled {
            ran_collector_count += 1;
            match units::parse_unit_state(&config, &sdc).await {
                Ok(units_stats) => monitord_stats.units = units_stats,
                Err(err) => error!("units stats failed: {:?}", err),
            }
        }

        if ran_collector_count < 1 {
            error!("No collectors ran. Exiting");
            std::process::exit(1);
        }

        let elapsed_runtime_ms = collect_start_time.elapsed().as_millis();
        info!("stat collection run took {}ms", elapsed_runtime_ms);
        print_stats(
            &config.monitord.key_prefix,
            &config.monitord.output_format,
            &monitord_stats,
        );
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
