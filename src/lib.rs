use std::path::PathBuf;
use std::str::FromStr;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use anyhow::Result;
use configparser::ini::Ini;
use log::error;
use log::info;

pub mod json;
pub mod networkd;
mod systemd_dbus;
pub mod units;

// TODO: Add other components as support is added
#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Eq, PartialEq)]
pub struct MonitordStats {
    pub networkd: networkd::NetworkdState,
    pub units: units::SystemdUnitStats,
}

fn read_config_bool(config: &Ini, section: String, key: String) -> bool {
    let option_bool = match config.getbool(&section, &key) {
        Ok(config_option_bool) => config_option_bool,
        Err(err) => panic!(
            "Unable to find '{}' key in '{}' section in config file: {}",
            key, section, err
        ),
    };
    match option_bool {
        Some(bool_value) => bool_value,
        None => {
            error!(
                "No value for '{}' in '{}' section ... assuming false",
                key, section
            );
            false
        }
    }
}

pub fn print_stats(config: Ini, stats: &MonitordStats) {
    let output_format = config
        .get("monitord", "output_format")
        .unwrap_or_else(|| "json".to_lowercase());
    match output_format.as_str() {
        "json" => println!("{}", serde_json::to_string(&stats).unwrap()),
        "json-flat" => println!("{}", json::flatten(stats)),
        "json-pretty" => println!("{}", serde_json::to_string_pretty(&stats).unwrap()),
        err => error!(
            "Unable to print output in {} format ... fix config ...",
            err
        ),
    }
}

pub fn stat_collector(config: Ini) -> Result<(), String> {
    let daemon_mode = read_config_bool(&config, String::from("monitord"), String::from("daemon"));
    let mut collect_interval_ms = 0;
    if daemon_mode {
        collect_interval_ms = match config.getuint("monitord", "daemon_stats_refresh_secs") {
            Ok(daemon_stats_refresh_secs) => daemon_stats_refresh_secs.unwrap(),
            Err(err) => {
                return Err(format!(
                    "Daemon mode is true in config and no daemon_stats_refresh_secs is set: {}",
                    err
                ))
            }
        };
    }

    let mut monitord_stats = MonitordStats::default();
    loop {
        let collect_start_time = Instant::now();
        let mut ran_collector_count: u8 = 0;

        info!("Starting stat collection run");

        // TODO: Move each collector into a function + thread
        // Run networkd collector if enabled
        if read_config_bool(&config, String::from("networkd"), String::from("enabled")) {
            ran_collector_count += 1;
            let networkd_start_path = PathBuf::from_str(
                config
                    .get("networkd", "link_state_dir")
                    .unwrap_or_else(|| String::from(networkd::NETWORKD_STATE_FILES))
                    .as_str(),
            );
            match networkd::parse_interface_state_files(
                networkd_start_path.unwrap(),
                networkd::NETWORKCTL_BINARY,
                vec![String::from("--json=short"), String::from("list")],
            ) {
                Ok(networkd_stats) => monitord_stats.networkd = networkd_stats,
                Err(err) => error!("networkd stats failed: {}", err),
            }
        }

        // Run units collector if enabled
        if read_config_bool(&config, String::from("units"), String::from("enabled")) {
            ran_collector_count += 1;
            match units::parse_unit_state() {
                Ok(units_stats) => monitord_stats.units = units_stats,
                Err(err) => error!("units stats failed: {}", err),
            }
        }

        if ran_collector_count < 1 {
            error!("No collectors ran. Exiting");
            std::process::exit(1);
        }

        let elapsed_runtime_ms: u64 = collect_start_time.elapsed().as_secs() * 1000;
        info!("stat collection run took {}ms", elapsed_runtime_ms);
        print_stats(config.clone(), &monitord_stats);
        if !daemon_mode {
            break;
        }
        let sleep_time_ms = collect_interval_ms - elapsed_runtime_ms;
        info!("stat collection sleeping for {}s 😴", sleep_time_ms / 1000);
        thread::sleep(Duration::from_millis(sleep_time_ms));
    }
    Ok(())
}
