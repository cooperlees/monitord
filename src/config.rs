use std::path::PathBuf;
use std::str::FromStr;

use configparser::ini::Ini;
use indexmap::map::IndexMap;
use int_enum::IntEnum;
use strum_macros::EnumString;
use tracing::error;

#[derive(Clone, Debug, Default, EnumString, Eq, IntEnum, PartialEq, strum_macros::Display)]
#[repr(u8)]
pub enum MonitordOutputFormat {
    #[default]
    #[strum(serialize = "json", serialize = "JSON", serialize = "Json")]
    Json = 0,
    #[strum(
        serialize = "json-flat",
        serialize = "json_flat",
        serialize = "jsonflat"
    )]
    JsonFlat = 1,
    #[strum(
        serialize = "json-pretty",
        serialize = "json_pretty",
        serialize = "jsonpretty"
    )]
    JsonPretty = 2,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MonitordConfig {
    pub dbus_address: String,
    pub daemon: bool,
    pub daemon_stats_refresh_secs: u64,
    pub key_prefix: String,
    pub output_format: MonitordOutputFormat,
}
impl Default for MonitordConfig {
    fn default() -> Self {
        MonitordConfig {
            dbus_address: crate::DEFAULT_DBUS_ADDRESS.into(),
            daemon: false,
            daemon_stats_refresh_secs: 30,
            key_prefix: "".to_string(),
            output_format: MonitordOutputFormat::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkdConfig {
    pub enabled: bool,
    pub link_state_dir: PathBuf,
}
impl Default for NetworkdConfig {
    fn default() -> Self {
        NetworkdConfig {
            enabled: false,
            link_state_dir: crate::networkd::NETWORKD_STATE_FILES.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pid1Config {
    pub enabled: bool,
}
impl Default for Pid1Config {
    fn default() -> Self {
        Pid1Config { enabled: true }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemStateConfig {
    pub enabled: bool,
}
impl Default for SystemStateConfig {
    fn default() -> Self {
        SystemStateConfig { enabled: true }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnitsConfig {
    pub enabled: bool,
    pub state_stats: bool,
    pub state_stats_allowlist: Vec<String>,
    pub state_stats_blocklist: Vec<String>,
}
impl Default for UnitsConfig {
    fn default() -> Self {
        UnitsConfig {
            enabled: true,
            state_stats: false,
            state_stats_allowlist: Vec::new(),
            state_stats_blocklist: Vec::new(),
        }
    }
}

/// Config struct
/// Each section represents an ini file section
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Config {
    pub monitord: MonitordConfig,
    pub networkd: NetworkdConfig,
    pub pid1: Pid1Config,
    pub services: Vec<String>,
    pub system_state: SystemStateConfig,
    pub units: UnitsConfig,
}

impl From<Ini> for Config {
    fn from(ini_config: Ini) -> Self {
        let mut config = Config::default();

        // [monitord] section
        if let Some(dbus_address) = ini_config.get("monitord", "dbus_address") {
            config.monitord.dbus_address = dbus_address;
        }
        config.monitord.daemon = read_config_bool(
            &ini_config,
            String::from("monitord"),
            String::from("daemon"),
        );
        if let Ok(Some(daemon_stats_refresh_secs)) =
            ini_config.getuint("monitord", "daemon_stats_refresh_secs")
        {
            config.monitord.daemon_stats_refresh_secs = daemon_stats_refresh_secs;
        }
        if let Some(key_prefix) = ini_config.get("monitord", "key_prefix") {
            config.monitord.key_prefix = key_prefix;
        }
        config.monitord.output_format = MonitordOutputFormat::from_str(
            &ini_config
                .get("monitord", "output_format")
                .expect("Need 'output_format' set in config"),
        )
        .expect("Need a valid value for the enum");

        // [networkd] section
        config.networkd.enabled = read_config_bool(
            &ini_config,
            String::from("networkd"),
            String::from("enabled"),
        );
        if let Some(link_state_dir) = ini_config.get("networkd", "link_state_dir") {
            config.networkd.link_state_dir = link_state_dir.into();
        }

        // [pid1] section
        config.pid1.enabled =
            read_config_bool(&ini_config, String::from("pid1"), String::from("enabled"));

        // [services] section
        let config_map = ini_config.get_map().unwrap_or(IndexMap::from([]));
        if let Some(services) = config_map.get("services") {
            config.services = services.keys().map(|s| s.to_string()).collect();
        }

        // [system-state] section
        config.system_state.enabled = read_config_bool(
            &ini_config,
            String::from("system-state"),
            String::from("enabled"),
        );

        // [units] section
        config.units.enabled =
            read_config_bool(&ini_config, String::from("units"), String::from("enabled"));
        config.units.state_stats = read_config_bool(
            &ini_config,
            String::from("units"),
            String::from("state_stats"),
        );
        if let Some(state_stats_allowlist) = config_map.get("units.state_stats.allowlist") {
            config.units.state_stats_allowlist = state_stats_allowlist
                .keys()
                .map(|s| s.to_string())
                .collect();
        }
        if let Some(state_stats_blocklist) = config_map.get("units.state_stats.blocklist") {
            config.units.state_stats_blocklist = state_stats_blocklist
                .keys()
                .map(|s| s.to_string())
                .collect();
        }

        config
    }
}

/// Helper function to read "bool" config options
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        assert!(Config::default().units.enabled)
    }
}
