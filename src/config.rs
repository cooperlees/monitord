use std::collections::HashSet;
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
    pub dbus_timeout: u64,
}
impl Default for MonitordConfig {
    fn default() -> Self {
        MonitordConfig {
            dbus_address: crate::DEFAULT_DBUS_ADDRESS.into(),
            daemon: false,
            daemon_stats_refresh_secs: 30,
            key_prefix: "".to_string(),
            output_format: MonitordOutputFormat::default(),
            dbus_timeout: 30,
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
pub struct TimersConfig {
    pub enabled: bool,
    pub allowlist: HashSet<String>,
    pub blocklist: HashSet<String>,
}
impl Default for TimersConfig {
    fn default() -> Self {
        TimersConfig {
            enabled: true,
            allowlist: HashSet::new(),
            blocklist: HashSet::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnitsConfig {
    pub enabled: bool,
    pub state_stats: bool,
    pub state_stats_allowlist: HashSet<String>,
    pub state_stats_blocklist: HashSet<String>,
    pub state_stats_time_in_state: bool,
}
impl Default for UnitsConfig {
    fn default() -> Self {
        UnitsConfig {
            enabled: true,
            state_stats: false,
            state_stats_allowlist: HashSet::new(),
            state_stats_blocklist: HashSet::new(),
            state_stats_time_in_state: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachinesConfig {
    pub enabled: bool,
    pub allowlist: HashSet<String>,
    pub blocklist: HashSet<String>,
}
impl Default for MachinesConfig {
    fn default() -> Self {
        MachinesConfig {
            enabled: true,
            allowlist: HashSet::new(),
            blocklist: HashSet::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DBusStatsConfig {
    pub enabled: bool,
    pub user_stats: bool,
    pub peer_stats: bool,
    pub cgroup_stats: bool,
}
impl Default for DBusStatsConfig {
    fn default() -> Self {
        DBusStatsConfig {
            enabled: true,
            user_stats: false,
            peer_stats: false,
            cgroup_stats: false,
        }
    }
}

/// Config struct
/// Each section represents an ini file section
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Config {
    pub machines: MachinesConfig,
    pub monitord: MonitordConfig,
    pub networkd: NetworkdConfig,
    pub pid1: Pid1Config,
    pub services: HashSet<String>,
    pub system_state: SystemStateConfig,
    pub timers: TimersConfig,
    pub units: UnitsConfig,
    pub dbus_stats: DBusStatsConfig,
}

impl From<Ini> for Config {
    fn from(ini_config: Ini) -> Self {
        let mut config = Config::default();

        // [monitord] section
        if let Some(dbus_address) = ini_config.get("monitord", "dbus_address") {
            config.monitord.dbus_address = dbus_address;
        }
        if let Ok(Some(dbus_timeout)) = ini_config.getuint("monitord", "dbus_timeout") {
            config.monitord.dbus_timeout = dbus_timeout;
        }
        config.monitord.daemon = read_config_bool(&ini_config, "monitord", "daemon");
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
        config.networkd.enabled = read_config_bool(&ini_config, "networkd", "enabled");
        if let Some(link_state_dir) = ini_config.get("networkd", "link_state_dir") {
            config.networkd.link_state_dir = link_state_dir.into();
        }

        // [pid1] section
        config.pid1.enabled = read_config_bool(&ini_config, "pid1", "enabled");

        // [services] section
        let config_map = ini_config.get_map().unwrap_or(IndexMap::from([]));
        if let Some(services) = config_map.get("services") {
            config.services = services.keys().map(|s| s.to_string()).collect();
        }

        // [system-state] section
        config.system_state.enabled = read_config_bool(&ini_config, "system-state", "enabled");

        // [timers] section
        config.timers.enabled = read_config_bool(&ini_config, "timers", "enabled");
        if let Some(timers_allowlist) = config_map.get("timers.allowlist") {
            config.timers.allowlist = timers_allowlist.keys().map(|s| s.to_string()).collect();
        }
        if let Some(timers_blocklist) = config_map.get("timers.blocklist") {
            config.timers.blocklist = timers_blocklist.keys().map(|s| s.to_string()).collect();
        }

        // [units] section
        config.units.enabled = read_config_bool(&ini_config, "units", "enabled");
        config.units.state_stats = read_config_bool(&ini_config, "units", "state_stats");
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
        config.units.state_stats_time_in_state =
            read_config_bool(&ini_config, "units", "state_stats_time_in_state");

        // [machines] section
        config.machines.enabled = read_config_bool(&ini_config, "machines", "enabled");
        if let Some(machines_allowlist) = config_map.get("machines.allowlist") {
            config.machines.allowlist = machines_allowlist.keys().map(|s| s.to_string()).collect();
        }
        if let Some(machines_blocklist) = config_map.get("machines.blocklist") {
            config.machines.blocklist = machines_blocklist.keys().map(|s| s.to_string()).collect();
        }

        // [dbus] section
        config.dbus_stats.enabled = read_config_bool(&ini_config, "dbus", "enabled");
        config.dbus_stats.user_stats = read_config_bool(&ini_config, "dbus", "user_stats");
        config.dbus_stats.peer_stats = read_config_bool(&ini_config, "dbus", "peer_stats");
        config.dbus_stats.cgroup_stats = read_config_bool(&ini_config, "dbus", "cgroup_stats");

        config
    }
}

/// Helper function to read "bool" config options
fn read_config_bool(config: &Ini, section: &str, key: &str) -> bool {
    let option_bool = match config.getbool(section, key) {
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
    use std::io::Write;

    use tempfile::NamedTempFile;

    use super::*;

    const FULL_CONFIG: &str = r###"
[monitord]
dbus_address = unix:path=/system_bus_socket
dbus_timeout = 2
daemon = true
daemon_stats_refresh_secs = 0
key_prefix = unittest
output_format = json-pretty

[networkd]
enabled = true
link_state_dir = /links

[pid1]
enabled = true

[services]
foo.service
bar.service

[system-state]
enabled = true

[timers]
enabled = true

[timers.allowlist]
foo.timer

[timers.blocklist]
bar.timer

[units]
enabled = true
state_stats = true
state_stats_time_in_state = true

[units.state_stats.allowlist]
foo.service

[units.state_stats.blocklist]
bar.service

[machines]
enabled = true

[machines.allowlist]
foo
bar

[machines.blocklist]
foo2

[dbus]
enabled = true
user_stats = true
peer_stats = true
cgroup_stats = true
"###;

    const MINIMAL_CONFIG: &str = r###"
[monitord]
output_format = json-flat
"###;

    #[test]
    fn test_default_config() {
        assert!(Config::default().units.enabled)
    }

    #[test]
    fn test_minimal_config() {
        let mut monitord_config = NamedTempFile::new().expect("Unable to make named tempfile");
        monitord_config
            .write_all(MINIMAL_CONFIG.as_bytes())
            .expect("Unable to write out temp config file");

        let mut ini_config = Ini::new();
        let _config_map = ini_config
            .load(monitord_config.path())
            .expect("Unable to load ini config");

        let expected_config: Config = ini_config.into();
        // See our one setting is not the default 'json' enum value
        assert_eq!(
            expected_config.monitord.output_format,
            MonitordOutputFormat::JsonFlat,
        );
        // See that one of the enabled bools are false
        assert!(!expected_config.networkd.enabled);
    }

    #[test]
    fn test_full_config() {
        let expected_config = Config {
            monitord: MonitordConfig {
                dbus_address: String::from("unix:path=/system_bus_socket"),
                daemon: true,
                daemon_stats_refresh_secs: u64::MIN,
                key_prefix: String::from("unittest"),
                output_format: MonitordOutputFormat::JsonPretty,
                dbus_timeout: 2 as u64,
            },
            networkd: NetworkdConfig {
                enabled: true,
                link_state_dir: "/links".into(),
            },
            pid1: Pid1Config { enabled: true },
            services: HashSet::from([String::from("foo.service"), String::from("bar.service")]),
            system_state: SystemStateConfig { enabled: true },
            timers: TimersConfig {
                enabled: true,
                allowlist: HashSet::from([String::from("foo.timer")]),
                blocklist: HashSet::from([String::from("bar.timer")]),
            },
            units: UnitsConfig {
                enabled: true,
                state_stats: true,
                state_stats_allowlist: HashSet::from([String::from("foo.service")]),
                state_stats_blocklist: HashSet::from([String::from("bar.service")]),
                state_stats_time_in_state: true,
            },
            machines: MachinesConfig {
                enabled: true,
                allowlist: HashSet::from([String::from("foo"), String::from("bar")]),
                blocklist: HashSet::from([String::from("foo2")]),
            },
            dbus_stats: DBusStatsConfig {
                enabled: true,
                user_stats: true,
                peer_stats: true,
                cgroup_stats: true,
            },
        };

        let mut monitord_config = NamedTempFile::new().expect("Unable to make named tempfile");
        monitord_config
            .write_all(FULL_CONFIG.as_bytes())
            .expect("Unable to write out temp config file");

        let mut ini_config = Ini::new();
        let _config_map = ini_config
            .load(monitord_config.path())
            .expect("Unable to load ini config");

        // See everything set / overloaded ...
        assert_eq!(expected_config, ini_config.into(),);
    }
}
