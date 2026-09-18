use std::collections::HashSet;
use std::path::PathBuf;
use std::str::FromStr;

use configparser::ini::Ini;
use indexmap::map::IndexMap;
use int_enum::IntEnum;
use strum_macros::EnumString;
use thiserror::Error;
use tracing::{error, warn};

#[derive(Error, Debug)]
pub enum MonitordConfigError {
    #[error("Invalid value for '{key}' in '{section}': {reason}")]
    InvalidValue {
        section: String,
        key: String,
        reason: String,
    },
    #[error("Missing key '{key}' in '{section}'")]
    MissingKey { section: String, key: String },
}

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
    /// Use varlink APIs for this collector when the global `[varlink]`
    /// switch is on. Default true: set false to keep this collector on
    /// D-Bus/files while the rest move to varlink.
    pub varlink: bool,
    pub link_state_dir: PathBuf,
}
impl Default for NetworkdConfig {
    fn default() -> Self {
        NetworkdConfig {
            enabled: false,
            varlink: true,
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
    /// Use varlink APIs for this collector when the global `[varlink]`
    /// switch is on. Default true: set false to keep this collector on
    /// D-Bus while the rest move to varlink. Also gates the always-on
    /// systemd version collection, which shares the `Manager.Describe` call.
    pub varlink: bool,
}
impl Default for SystemStateConfig {
    fn default() -> Self {
        SystemStateConfig {
            enabled: true,
            varlink: true,
        }
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
    /// Use varlink APIs for this collector when the global `[varlink]`
    /// switch is on. Default true: set false to keep this collector on
    /// D-Bus while the rest move to varlink.
    pub varlink: bool,
    pub state_stats: bool,
    pub state_stats_allowlist: HashSet<String>,
    pub state_stats_blocklist: HashSet<String>,
    pub state_stats_time_in_state: bool,
    pub ignore_inactive_oneshot_services: bool,
    pub unit_files: bool,
    /// Max number of units whose D-Bus work runs concurrently in the per-unit
    /// collection loop. Bounded (rather than unbounded) so a burst of
    /// simultaneous D-Bus calls doesn't itself worsen host-level IPC
    /// contention on hosts where per-call latency is already elevated.
    pub per_unit_concurrency: u64,
    /// Number of slowest units (by per-unit collection duration) to record in
    /// `UnitsCollectionTimings::slowest_units`. Set to 0 to disable.
    pub slowest_units_count: u64,
}
impl Default for UnitsConfig {
    fn default() -> Self {
        UnitsConfig {
            enabled: true,
            varlink: true,
            state_stats: false,
            state_stats_allowlist: HashSet::new(),
            state_stats_blocklist: HashSet::new(),
            state_stats_time_in_state: true,
            ignore_inactive_oneshot_services: true,
            unit_files: true,
            per_unit_concurrency: 8,
            slowest_units_count: 5,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachinesConfig {
    pub enabled: bool,
    /// Use varlink APIs inside containers when the global `[varlink]`
    /// switch is on. Default true: set false to keep all container
    /// collection on D-Bus. Container paths additionally require the
    /// matching collector section's own `varlink` toggle.
    pub varlink: bool,
    pub allowlist: HashSet<String>,
    pub blocklist: HashSet<String>,
}
impl Default for MachinesConfig {
    fn default() -> Self {
        MachinesConfig {
            enabled: true,
            varlink: true,
            allowlist: HashSet::new(),
            blocklist: HashSet::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DBusStatsConfig {
    pub enabled: bool,
    pub stale_fd_stats: bool,

    pub user_stats: bool,
    pub user_allowlist: HashSet<String>,
    pub user_blocklist: HashSet<String>,

    pub peer_stats: bool,
    pub peer_well_known_names_only: bool,
    pub peer_allowlist: HashSet<String>,
    pub peer_blocklist: HashSet<String>,
    /// Max number of well-known bus names whose owner lookup runs concurrently
    /// when resolving peer names. Bounded (rather than unbounded) for the same
    /// reason as `UnitsConfig::per_unit_concurrency`: a burst of simultaneous
    /// D-Bus calls can itself worsen host-level IPC contention.
    pub peer_name_concurrency: u64,

    pub cgroup_stats: bool,
    pub cgroup_allowlist: HashSet<String>,
    pub cgroup_blocklist: HashSet<String>,
}
impl Default for DBusStatsConfig {
    fn default() -> Self {
        DBusStatsConfig {
            enabled: true,
            stale_fd_stats: true,

            user_stats: false,
            user_allowlist: HashSet::new(),
            user_blocklist: HashSet::new(),

            peer_stats: false,
            peer_well_known_names_only: false,
            peer_allowlist: HashSet::new(),
            peer_blocklist: HashSet::new(),
            peer_name_concurrency: 8,

            cgroup_stats: false,
            cgroup_allowlist: HashSet::new(),
            cgroup_blocklist: HashSet::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootBlameConfig {
    pub enabled: bool,
    /// Use varlink APIs for this collector when the global `[varlink]`
    /// switch is on. Default true: set false to keep this collector on
    /// D-Bus while the rest move to varlink.
    pub varlink: bool,
    pub cache_enabled: bool,
    pub cache_dir: String,
    pub num_slowest_units: u64,
    pub allowlist: HashSet<String>,
    pub blocklist: HashSet<String>,
}
impl Default for BootBlameConfig {
    fn default() -> Self {
        BootBlameConfig {
            enabled: false,
            varlink: true,
            cache_enabled: true,
            cache_dir: "/run/monitord".to_string(),
            num_slowest_units: 5,
            allowlist: HashSet::new(),
            blocklist: HashSet::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VerifyConfig {
    pub enabled: bool,
    pub allowlist: HashSet<String>,
    pub blocklist: HashSet<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VarlinkConfig {
    /// Global varlink master switch. Each varlink-capable collector section
    /// has its own `varlink` opt-out; a collector uses varlink only when
    /// both this and its section toggle are true.
    pub enabled: bool,
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
    pub boot_blame: BootBlameConfig,
    pub verify: VerifyConfig,
    pub varlink: VarlinkConfig,
}

impl Config {
    /// Whether collectors gated on the given section toggles may use varlink.
    ///
    /// The global `[varlink]` master switch ANDed with every section toggle
    /// passed. Container paths additionally pass `machines.varlink`, since a
    /// container may run older systemd without the varlink APIs.
    pub fn use_varlink(&self, sections: &[bool]) -> bool {
        self.varlink.enabled && sections.iter().all(|toggle| *toggle)
    }
}

impl TryFrom<Ini> for Config {
    type Error = MonitordConfigError;

    fn try_from(ini_config: Ini) -> Result<Self, MonitordConfigError> {
        let mut config = Config::default();

        // [monitord] section
        if let Some(dbus_address) = ini_config.get("monitord", "dbus_address") {
            config.monitord.dbus_address = dbus_address;
        }
        if let Ok(Some(dbus_timeout)) = ini_config.getuint("monitord", "dbus_timeout") {
            config.monitord.dbus_timeout = dbus_timeout;
        }
        config.monitord.daemon = read_config_bool(&ini_config, "monitord", "daemon")?;
        if let Ok(Some(daemon_stats_refresh_secs)) =
            ini_config.getuint("monitord", "daemon_stats_refresh_secs")
        {
            config.monitord.daemon_stats_refresh_secs = daemon_stats_refresh_secs;
        }
        if let Some(key_prefix) = ini_config.get("monitord", "key_prefix") {
            config.monitord.key_prefix = key_prefix;
        }
        let output_format_str = ini_config.get("monitord", "output_format").ok_or_else(|| {
            MonitordConfigError::MissingKey {
                section: "monitord".into(),
                key: "output_format".into(),
            }
        })?;
        config.monitord.output_format = MonitordOutputFormat::from_str(&output_format_str)
            .map_err(|e| MonitordConfigError::InvalidValue {
                section: "monitord".into(),
                key: "output_format".into(),
                reason: e.to_string(),
            })?;

        // [networkd] section
        config.networkd.enabled = read_config_bool(&ini_config, "networkd", "enabled")?;
        if let Some(varlink) = read_config_optional_bool(&ini_config, "networkd", "varlink")? {
            config.networkd.varlink = varlink;
        }
        if let Some(link_state_dir) = ini_config.get("networkd", "link_state_dir") {
            config.networkd.link_state_dir = link_state_dir.into();
        }

        // [pid1] section
        config.pid1.enabled = read_config_bool(&ini_config, "pid1", "enabled")?;

        // [services] section
        let config_map = ini_config.get_map().unwrap_or(IndexMap::from([]));
        if let Some(services) = config_map.get("services") {
            config.services = services.keys().map(|s| s.to_string()).collect();
        }

        // [system-state] section
        config.system_state.enabled = read_config_bool(&ini_config, "system-state", "enabled")?;
        if let Some(varlink) = read_config_optional_bool(&ini_config, "system-state", "varlink")? {
            config.system_state.varlink = varlink;
        }

        // [timers] section
        config.timers.enabled = read_config_bool(&ini_config, "timers", "enabled")?;
        if let Some(timers_allowlist) = config_map.get("timers.allowlist") {
            config.timers.allowlist = timers_allowlist.keys().map(|s| s.to_string()).collect();
        }
        if let Some(timers_blocklist) = config_map.get("timers.blocklist") {
            config.timers.blocklist = timers_blocklist.keys().map(|s| s.to_string()).collect();
        }

        // [units] section
        config.units.enabled = read_config_bool(&ini_config, "units", "enabled")?;
        if let Some(varlink) = read_config_optional_bool(&ini_config, "units", "varlink")? {
            config.units.varlink = varlink;
        }
        config.units.state_stats = read_config_bool(&ini_config, "units", "state_stats")?;
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
            read_config_bool(&ini_config, "units", "state_stats_time_in_state")?;
        if let Some(ignore_inactive_oneshot_services) =
            read_config_optional_bool(&ini_config, "units", "ignore_inactive_oneshot_services")?
        {
            config.units.ignore_inactive_oneshot_services = ignore_inactive_oneshot_services;
        }
        if let Some(unit_files) = read_config_optional_bool(&ini_config, "units", "unit_files")? {
            config.units.unit_files = unit_files;
        }
        if let Ok(Some(per_unit_concurrency)) = ini_config.getuint("units", "per_unit_concurrency")
        {
            config.units.per_unit_concurrency = per_unit_concurrency;
        }
        if let Ok(Some(slowest_units_count)) = ini_config.getuint("units", "slowest_units_count") {
            config.units.slowest_units_count = slowest_units_count;
        }

        // [machines] section
        config.machines.enabled = read_config_bool(&ini_config, "machines", "enabled")?;
        if let Some(varlink) = read_config_optional_bool(&ini_config, "machines", "varlink")? {
            config.machines.varlink = varlink;
        }
        if let Some(machines_allowlist) = config_map.get("machines.allowlist") {
            config.machines.allowlist = machines_allowlist.keys().map(|s| s.to_string()).collect();
        }
        if let Some(machines_blocklist) = config_map.get("machines.blocklist") {
            config.machines.blocklist = machines_blocklist.keys().map(|s| s.to_string()).collect();
        }

        // [dbus] section
        config.dbus_stats.enabled = read_config_bool(&ini_config, "dbus", "enabled")?;
        if let Some(stale_fd_stats) =
            read_config_optional_bool(&ini_config, "dbus", "stale_fd_stats")?
        {
            config.dbus_stats.stale_fd_stats = stale_fd_stats;
        }

        config.dbus_stats.user_stats = read_config_bool(&ini_config, "dbus", "user_stats")?;
        if let Some(user_allowlist) = config_map.get("dbus.user.allowlist") {
            config.dbus_stats.user_allowlist =
                user_allowlist.keys().map(|s| s.to_string()).collect();
        }
        if let Some(user_blocklist) = config_map.get("dbus.user.blocklist") {
            config.dbus_stats.user_blocklist =
                user_blocklist.keys().map(|s| s.to_string()).collect();
        }

        config.dbus_stats.peer_stats = read_config_bool(&ini_config, "dbus", "peer_stats")?;
        config.dbus_stats.peer_well_known_names_only =
            read_config_bool(&ini_config, "dbus", "peer_well_known_names_only")?;
        if let Some(peer_allowlist) = config_map.get("dbus.peer.allowlist") {
            config.dbus_stats.peer_allowlist =
                peer_allowlist.keys().map(|s| s.to_string()).collect();
        }
        if let Some(peer_blocklist) = config_map.get("dbus.peer.blocklist") {
            config.dbus_stats.peer_blocklist =
                peer_blocklist.keys().map(|s| s.to_string()).collect();
        }
        if let Ok(Some(peer_name_concurrency)) = ini_config.getuint("dbus", "peer_name_concurrency")
        {
            config.dbus_stats.peer_name_concurrency = peer_name_concurrency;
        }

        config.dbus_stats.cgroup_stats = read_config_bool(&ini_config, "dbus", "cgroup_stats")?;
        if let Some(cgroup_allowlist) = config_map.get("dbus.cgroup.allowlist") {
            config.dbus_stats.cgroup_allowlist =
                cgroup_allowlist.keys().map(|s| s.to_string()).collect();
        }
        if let Some(cgroup_blocklist) = config_map.get("dbus.cgroup.blocklist") {
            config.dbus_stats.cgroup_blocklist =
                cgroup_blocklist.keys().map(|s| s.to_string()).collect();
        }

        // [boot] section
        config.boot_blame.enabled = read_config_bool(&ini_config, "boot", "enabled")?;
        if let Some(varlink) = read_config_optional_bool(&ini_config, "boot", "varlink")? {
            config.boot_blame.varlink = varlink;
        }
        if let Some(cache_enabled) =
            read_config_optional_bool(&ini_config, "boot", "cache_enabled")?
        {
            config.boot_blame.cache_enabled = cache_enabled;
        }
        if let Some(cache_dir) = ini_config.get("boot", "cache_dir") {
            config.boot_blame.cache_dir = cache_dir;
        }
        if let Ok(Some(num_slowest_units)) = ini_config.getuint("boot", "num_slowest_units") {
            config.boot_blame.num_slowest_units = num_slowest_units;
        }
        if let Some(boot_allowlist) = config_map.get("boot.allowlist") {
            config.boot_blame.allowlist = boot_allowlist.keys().map(|s| s.to_string()).collect();
        }
        if let Some(boot_blocklist) = config_map.get("boot.blocklist") {
            config.boot_blame.blocklist = boot_blocklist.keys().map(|s| s.to_string()).collect();
        }

        // [verify] section
        config.verify.enabled = read_config_bool(&ini_config, "verify", "enabled")?;
        if let Some(verify_allowlist) = config_map.get("verify.allowlist") {
            config.verify.allowlist = verify_allowlist.keys().map(|s| s.to_string()).collect();
        }
        if let Some(verify_blocklist) = config_map.get("verify.blocklist") {
            config.verify.blocklist = verify_blocklist.keys().map(|s| s.to_string()).collect();
        }

        // [varlink] section
        config.varlink.enabled = read_config_bool(&ini_config, "varlink", "enabled")?;

        for entry in unknown_config_entries(&config_map) {
            warn!("Ignoring {entry}; check for a typo'd key or section");
        }

        Ok(config)
    }
}

/// Fixed-key sections and every key monitord reads from each.
///
/// Data sections (`[services]`, `*.allowlist`, `*.blocklist`) take arbitrary
/// entries and are exempt; anything else unknown is warned about so a typo'd
/// key parses loudly instead of silently doing nothing.
const KNOWN_SECTION_KEYS: &[(&str, &[&str])] = &[
    (
        "monitord",
        &[
            "dbus_address",
            "dbus_timeout",
            "daemon",
            "daemon_stats_refresh_secs",
            "key_prefix",
            "output_format",
        ],
    ),
    ("networkd", &["enabled", "varlink", "link_state_dir"]),
    ("pid1", &["enabled"]),
    ("system-state", &["enabled", "varlink"]),
    ("timers", &["enabled"]),
    (
        "units",
        &[
            "enabled",
            "varlink",
            "state_stats",
            "state_stats_time_in_state",
            "ignore_inactive_oneshot_services",
            "unit_files",
            "per_unit_concurrency",
            "slowest_units_count",
        ],
    ),
    ("machines", &["enabled", "varlink"]),
    (
        "dbus",
        &[
            "enabled",
            "stale_fd_stats",
            "user_stats",
            "peer_stats",
            "peer_well_known_names_only",
            "peer_name_concurrency",
            "cgroup_stats",
        ],
    ),
    (
        "boot",
        &[
            "enabled",
            "varlink",
            "cache_enabled",
            "cache_dir",
            "num_slowest_units",
        ],
    ),
    ("verify", &["enabled"]),
    ("varlink", &["enabled"]),
];

/// Sections whose entries are data (unit/machine names), not fixed keys.
const DATA_SECTIONS: &[&str] = &[
    "services",
    "timers.allowlist",
    "timers.blocklist",
    "units.state_stats.allowlist",
    "units.state_stats.blocklist",
    "machines.allowlist",
    "machines.blocklist",
    "dbus.user.allowlist",
    "dbus.user.blocklist",
    "dbus.peer.allowlist",
    "dbus.peer.blocklist",
    "dbus.cgroup.allowlist",
    "dbus.cgroup.blocklist",
    "boot.allowlist",
    "boot.blocklist",
    "verify.allowlist",
    "verify.blocklist",
];

/// Unrecognized sections and keys, for warn-on-typo diagnostics.
///
/// Pure (returns messages) so tests can assert on it; the caller logs them.
fn unknown_config_entries(
    config_map: &IndexMap<String, IndexMap<String, Option<String>>>,
) -> Vec<String> {
    let mut unknown = Vec::new();
    for (section, keys) in config_map {
        if DATA_SECTIONS.contains(&section.as_str()) {
            continue;
        }
        match KNOWN_SECTION_KEYS.iter().find(|(name, _)| name == section) {
            None => unknown.push(format!("unknown section [{section}]")),
            Some((_, known_keys)) => {
                for key in keys.keys() {
                    if !known_keys.contains(&key.as_str()) {
                        unknown.push(format!("unknown key '{key}' in [{section}]"));
                    }
                }
            }
        }
    }
    unknown
}

/// Helper function to read "bool" config options
fn read_config_bool(config: &Ini, section: &str, key: &str) -> Result<bool, MonitordConfigError> {
    let option_bool =
        config
            .getbool(section, key)
            .map_err(|err| MonitordConfigError::InvalidValue {
                section: section.into(),
                key: key.into(),
                reason: err,
            })?;
    match option_bool {
        Some(bool_value) => Ok(bool_value),
        None => {
            error!(
                "No value for '{}' in '{}' section ... assuming false",
                key, section
            );
            Ok(false)
        }
    }
}

/// Helper function to read optional bool config options while preserving field defaults
fn read_config_optional_bool(
    config: &Ini,
    section: &str,
    key: &str,
) -> Result<Option<bool>, MonitordConfigError> {
    config
        .getbool(section, key)
        .map_err(|err| MonitordConfigError::InvalidValue {
            section: section.into(),
            key: key.into(),
            reason: err,
        })
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
varlink = false
link_state_dir = /links

[pid1]
enabled = true

[services]
foo.service
bar.service

[system-state]
enabled = true
varlink = false

[timers]
enabled = true

[timers.allowlist]
foo.timer

[timers.blocklist]
bar.timer

[units]
enabled = true
varlink = false
state_stats = true
state_stats_time_in_state = true
ignore_inactive_oneshot_services = true
unit_files = true
per_unit_concurrency = 16
slowest_units_count = 3

[units.state_stats.allowlist]
foo.service

[units.state_stats.blocklist]
bar.service

[machines]
enabled = true
varlink = false

[machines.allowlist]
foo
bar

[machines.blocklist]
foo2

[dbus]
enabled = true
stale_fd_stats = true
user_stats = true
peer_stats = true
peer_well_known_names_only = true
peer_name_concurrency = 12
cgroup_stats = true

[dbus.user.allowlist]
foo
bar

[dbus.user.blocklist]
foo2

[dbus.peer.allowlist]
foo
bar

[dbus.peer.blocklist]
foo2

[dbus.cgroup.allowlist]
foo
bar

[dbus.cgroup.blocklist]
foo2

[boot]
enabled = true
varlink = true
cache_enabled = false
cache_dir = /tmp/monitord-test
num_slowest_units = 10

[boot.allowlist]
foo.service

[boot.blocklist]
bar.service

[varlink]
enabled = true
"###;

    const MINIMAL_CONFIG: &str = r###"
[monitord]
output_format = json-flat
"###;

    #[test]
    fn test_default_config() {
        assert!(Config::default().units.enabled);
        // Per-section varlink toggles default to following the global switch
        let default_config = Config::default();
        assert!(default_config.units.varlink);
        assert!(default_config.networkd.varlink);
        assert!(default_config.system_state.varlink);
        assert!(default_config.machines.varlink);
        assert!(default_config.boot_blame.varlink);
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

        let expected_config: Config = ini_config.try_into().expect("Failed to parse config");
        // See our one setting is not the default 'json' enum value
        assert_eq!(
            expected_config.monitord.output_format,
            MonitordOutputFormat::JsonFlat,
        );
        // See that one of the enabled bools are false
        assert!(!expected_config.networkd.enabled);
        // Boot cache defaults to enabled when not explicitly configured
        assert!(expected_config.boot_blame.cache_enabled);
        // Oneshot inactive services are ignored by default
        assert!(expected_config.units.ignore_inactive_oneshot_services);
    }

    #[test]
    fn test_units_ignore_inactive_oneshot_services_override() {
        let units_override_config = r###"
[monitord]
output_format = json

[units]
ignore_inactive_oneshot_services = false
"###;
        let mut monitord_config = NamedTempFile::new().expect("Unable to make named tempfile");
        monitord_config
            .write_all(units_override_config.as_bytes())
            .expect("Unable to write out temp config file");

        let mut ini_config = Ini::new();
        let _config_map = ini_config
            .load(monitord_config.path())
            .expect("Unable to load ini config");

        let parsed_config: Config = ini_config.try_into().expect("Failed to parse config");
        assert!(!parsed_config.units.ignore_inactive_oneshot_services);
    }

    #[test]
    fn test_units_per_unit_concurrency_override() {
        let units_override_config = r###"
[monitord]
output_format = json

[units]
per_unit_concurrency = 32
slowest_units_count = 0
"###;
        let mut monitord_config = NamedTempFile::new().expect("Unable to make named tempfile");
        monitord_config
            .write_all(units_override_config.as_bytes())
            .expect("Unable to write out temp config file");

        let mut ini_config = Ini::new();
        let _config_map = ini_config
            .load(monitord_config.path())
            .expect("Unable to load ini config");

        let parsed_config: Config = ini_config.try_into().expect("Failed to parse config");
        assert_eq!(parsed_config.units.per_unit_concurrency, 32);
        assert_eq!(parsed_config.units.slowest_units_count, 0);
    }

    #[test]
    fn test_use_varlink_conjunction() {
        // The conjunction is the point of the per-section toggles: every
        // gate must agree, so a global-off with sections on stays off, and
        // any single opt-out disables its collector (plus the container
        // triple-AND through machines.varlink).
        let mut config = Config::default();
        assert!(!config.use_varlink(&[true]));
        config.varlink.enabled = true;
        assert!(config.use_varlink(&[true]));
        assert!(!config.use_varlink(&[false]));
        assert!(!config.use_varlink(&[true, false]));
        assert!(config.use_varlink(&[true, true]));
        config.varlink.enabled = false;
        assert!(!config.use_varlink(&[true, true]));
    }

    #[test]
    fn test_unknown_config_entries() {
        // Typo'd keys and sections parse clean, so they must at least warn:
        // [timers] varlink is the trap (timers follow [units]), and data
        // sections must stay exempt since their entries are unit names.
        let typo_config = r###"
[monitord]
output_format = json

[units]
varlnik = false

[timers]
varlink = false

[services]
foo.service

[bogus]
key = value
"###;
        let mut monitord_config = NamedTempFile::new().expect("Unable to make named tempfile");
        monitord_config
            .write_all(typo_config.as_bytes())
            .expect("Unable to write out temp config file");

        let mut ini_config = Ini::new();
        ini_config
            .load(monitord_config.path())
            .expect("Unable to load ini config");

        let unknown = unknown_config_entries(&ini_config.get_map().expect("config map"));
        assert!(unknown.contains(&"unknown key 'varlnik' in [units]".to_string()));
        assert!(unknown.contains(&"unknown key 'varlink' in [timers]".to_string()));
        assert!(unknown.contains(&"unknown section [bogus]".to_string()));
        assert_eq!(unknown.len(), 3);
    }

    #[test]
    fn test_per_section_varlink_toggle_defaults_and_override() {
        // Only [units] opts out; every other section keeps the default true,
        // so collectors can move to varlink one at a time.
        let varlink_override_config = r###"
[monitord]
output_format = json

[varlink]
enabled = true

[units]
varlink = false
"###;
        let mut monitord_config = NamedTempFile::new().expect("Unable to make named tempfile");
        monitord_config
            .write_all(varlink_override_config.as_bytes())
            .expect("Unable to write out temp config file");

        let mut ini_config = Ini::new();
        let _config_map = ini_config
            .load(monitord_config.path())
            .expect("Unable to load ini config");

        let parsed_config: Config = ini_config.try_into().expect("Failed to parse config");
        assert!(parsed_config.varlink.enabled);
        assert!(!parsed_config.units.varlink);
        assert!(parsed_config.networkd.varlink);
        assert!(parsed_config.system_state.varlink);
        assert!(parsed_config.machines.varlink);
        assert!(parsed_config.boot_blame.varlink);
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
                varlink: false,
                link_state_dir: "/links".into(),
            },
            pid1: Pid1Config { enabled: true },
            services: HashSet::from([String::from("foo.service"), String::from("bar.service")]),
            system_state: SystemStateConfig {
                enabled: true,
                varlink: false,
            },
            timers: TimersConfig {
                enabled: true,
                allowlist: HashSet::from([String::from("foo.timer")]),
                blocklist: HashSet::from([String::from("bar.timer")]),
            },
            units: UnitsConfig {
                enabled: true,
                varlink: false,
                state_stats: true,
                state_stats_allowlist: HashSet::from([String::from("foo.service")]),
                state_stats_blocklist: HashSet::from([String::from("bar.service")]),
                state_stats_time_in_state: true,
                ignore_inactive_oneshot_services: true,
                unit_files: true,
                per_unit_concurrency: 16,
                slowest_units_count: 3,
            },
            machines: MachinesConfig {
                enabled: true,
                varlink: false,
                allowlist: HashSet::from([String::from("foo"), String::from("bar")]),
                blocklist: HashSet::from([String::from("foo2")]),
            },
            dbus_stats: DBusStatsConfig {
                enabled: true,
                stale_fd_stats: true,
                user_stats: true,
                user_allowlist: HashSet::from([String::from("foo"), String::from("bar")]),
                user_blocklist: HashSet::from([String::from("foo2")]),
                peer_stats: true,
                peer_well_known_names_only: true,
                peer_allowlist: HashSet::from([String::from("foo"), String::from("bar")]),
                peer_blocklist: HashSet::from([String::from("foo2")]),
                peer_name_concurrency: 12,
                cgroup_stats: true,
                cgroup_allowlist: HashSet::from([String::from("foo"), String::from("bar")]),
                cgroup_blocklist: HashSet::from([String::from("foo2")]),
            },
            boot_blame: BootBlameConfig {
                enabled: true,
                // Explicit true (the other sections pin false): proves the
                // key is actually read rather than the default shining
                // through.
                varlink: true,
                cache_enabled: false,
                cache_dir: "/tmp/monitord-test".to_string(),
                num_slowest_units: 10,
                allowlist: HashSet::from([String::from("foo.service")]),
                blocklist: HashSet::from([String::from("bar.service")]),
            },
            verify: VerifyConfig {
                enabled: false,
                allowlist: HashSet::new(),
                blocklist: HashSet::new(),
            },
            varlink: VarlinkConfig { enabled: true },
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
        let actual_config: Config = ini_config.try_into().expect("Failed to parse config");
        assert_eq!(expected_config, actual_config);
    }

    #[test]
    fn test_invalid_config_returns_error() {
        let invalid_config = "[monitord]\ndaemon = notabool\noutput_format = json\n";
        let mut monitord_config = NamedTempFile::new().expect("Unable to make named tempfile");
        monitord_config
            .write_all(invalid_config.as_bytes())
            .expect("Unable to write out temp config file");

        let mut ini_config = Ini::new();
        let _config_map = ini_config
            .load(monitord_config.path())
            .expect("Unable to load ini config");

        let result: Result<Config, _> = ini_config.try_into();
        assert!(result.is_err());
    }
}
