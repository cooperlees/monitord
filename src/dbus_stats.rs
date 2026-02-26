//! # dbus_stats module
//!
//! Handle getting statistics of our Dbus daemon/broker

use std::collections::HashMap;
use std::fs;
use std::io;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::RwLock;
use tracing::error;
use uzers::get_user_by_uid;
use zbus::fdo::{DBusProxy, StatsProxy};
use zbus::names::BusName;
use zvariant::{Dict, OwnedValue, Value};

use crate::MachineStats;

#[derive(Error, Debug)]
pub enum MonitordDbusStatsError {
    #[error("D-Bus error: {0}")]
    ZbusError(#[from] zbus::Error),
    #[error("D-Bus fdo error: {0}")]
    FdoError(#[from] zbus::fdo::Error),
}

// Unfortunately, various DBus daemons (ex: dbus-broker and dbus-daemon)
// represent stats differently. Moreover, the stats vary across versions of the same daemon.
// Hence, the code uses flexible approach providing max available information.

/// Per-peer resource accounting from dbus-broker's PeerAccounting stats.
/// Each peer represents a single D-Bus connection identified by a unique bus name.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct DBusBrokerPeerAccounting {
    /// Unique D-Bus connection name (e.g. ":1.42")
    pub id: String,
    /// Well-known bus name owned by this peer, if any (e.g. "org.freedesktop.NetworkManager")
    pub well_known_name: Option<String>,

    // credentials
    /// Unix UID of the process owning this D-Bus connection
    pub unix_user_id: Option<u32>,
    /// PID of the process owning this D-Bus connection
    pub process_id: Option<u32>,
    /// Unix supplementary group IDs of the process owning this connection
    pub unix_group_ids: Option<Vec<u32>>,
    // ignoring LinuxSecurityLabel
    // pub linux_security_label: Option<String>,

    // stats
    /// Number of bus name objects held by this peer
    pub name_objects: Option<u32>,
    /// Bytes consumed by match rules registered by this peer
    pub match_bytes: Option<u32>,
    /// Number of match rules registered by this peer for signal filtering
    pub matches: Option<u32>,
    /// Number of pending reply objects (outstanding method calls awaiting replies)
    pub reply_objects: Option<u32>,
    /// Total bytes received by this peer from the bus
    pub incoming_bytes: Option<u32>,
    /// Total file descriptors received by this peer via D-Bus fd-passing
    pub incoming_fds: Option<u32>,
    /// Total bytes sent by this peer to the bus
    pub outgoing_bytes: Option<u32>,
    /// Total file descriptors sent by this peer via D-Bus fd-passing
    pub outgoing_fds: Option<u32>,
    /// Bytes used for D-Bus activation requests by this peer
    pub activation_request_bytes: Option<u32>,
    /// File descriptors used for D-Bus activation requests by this peer
    pub activation_request_fds: Option<u32>,
}

impl DBusBrokerPeerAccounting {
    /// Returns true if the peer has a well-known name
    pub fn has_well_known_name(&self) -> bool {
        self.well_known_name.is_some()
    }

    /// Returns the well-known name if present, otherwise falls back to the unique D-Bus connection ID
    pub fn get_name(&self) -> String {
        self.well_known_name.clone().unwrap_or_else(|| self.id.clone())
    }

    pub fn get_cgroup_name(&self) -> Result<String, io::Error> {
        let pid = self
            .process_id
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing process_id"))?;

        let path = format!("/proc/{}/cgroup", pid);
        let content = fs::read_to_string(&path)?;

        // ex: 0::/system.slice/metalos.classic.metald.service
        let cgroup = content.strip_prefix("0::").ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "unexpected cgroup format")
        })?;

        Ok(cgroup.trim().trim_matches('/').replace('/', "-"))
    }
}

/// Aggregated D-Bus resource accounting grouped by cgroup.
/// Not directly present in dbus-broker stats; computed by summing peer stats that share a cgroup.
/// Grouping by cgroup reduces metric cardinality while still identifying abusive clients.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct DBusBrokerCGroupAccounting {
    /// Cgroup path with slashes replaced by dashes (e.g. "system.slice-sshd.service")
    pub name: String,

    // stats (aggregated sums across all peers in this cgroup)
    /// Total bus name objects held by peers in this cgroup
    pub name_objects: Option<u32>,
    /// Total bytes consumed by match rules from peers in this cgroup
    pub match_bytes: Option<u32>,
    /// Total match rules registered by peers in this cgroup
    pub matches: Option<u32>,
    /// Total pending reply objects from peers in this cgroup
    pub reply_objects: Option<u32>,
    /// Total bytes received by peers in this cgroup
    pub incoming_bytes: Option<u32>,
    /// Total file descriptors received by peers in this cgroup
    pub incoming_fds: Option<u32>,
    /// Total bytes sent by peers in this cgroup
    pub outgoing_bytes: Option<u32>,
    /// Total file descriptors sent by peers in this cgroup
    pub outgoing_fds: Option<u32>,
    /// Total activation request bytes from peers in this cgroup
    pub activation_request_bytes: Option<u32>,
    /// Total activation request file descriptors from peers in this cgroup
    pub activation_request_fds: Option<u32>,
}

impl DBusBrokerCGroupAccounting {
    pub fn combine_with_peer(&mut self, peer: &DBusBrokerPeerAccounting) {
        fn sum(a: &mut Option<u32>, b: &Option<u32>) {
            *a = match (a.take(), b) {
                (Some(x), Some(y)) => Some(x + y),
                (Some(x), None) => Some(x),
                (None, Some(y)) => Some(*y),
                (None, None) => None,
            };
        }

        sum(&mut self.name_objects, &peer.name_objects);
        sum(&mut self.match_bytes, &peer.match_bytes);
        sum(&mut self.matches, &peer.matches);
        sum(&mut self.reply_objects, &peer.reply_objects);
        sum(&mut self.incoming_bytes, &peer.incoming_bytes);
        sum(&mut self.incoming_fds, &peer.incoming_fds);
        sum(&mut self.outgoing_bytes, &peer.outgoing_bytes);
        sum(&mut self.outgoing_fds, &peer.outgoing_fds);
        sum(
            &mut self.activation_request_bytes,
            &peer.activation_request_bytes,
        );
        sum(
            &mut self.activation_request_fds,
            &peer.activation_request_fds,
        );
    }
}

/// Current/maximum resource pair as reported by dbus-broker's UserAccounting.
/// Note: dbus-broker stores the current value in inverted form; actual usage = max - cur.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct CurMaxPair {
    /// Remaining quota (inverted: actual usage = max - cur)
    pub cur: u32,
    /// Maximum allowed quota for this resource
    pub max: u32,
}

impl CurMaxPair {
    pub fn get_usage(&self) -> u32 {
        // There is a theoretical possibility of max < cur due to various factors.
        // I'll leave it for now to avoid premature optimizations.
        self.max - self.cur
    }
}

/// Per-user aggregated D-Bus resource limits and usage from dbus-broker's UserAccounting.
/// Each entry tracks quota consumption across all connections belonging to a Unix user.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct DBusBrokerUserAccounting {
    /// Unix user ID this accounting entry belongs to
    pub uid: u32,
    pub username: String,

    /// Message byte quota: remaining (cur) and maximum (max) allowed bytes across all connections
    pub bytes: Option<CurMaxPair>,
    /// File descriptor quota: remaining (cur) and maximum (max) allowed FDs across all connections
    pub fds: Option<CurMaxPair>,
    /// Match rule quota: remaining (cur) and maximum (max) allowed match rules across all connections
    pub matches: Option<CurMaxPair>,
    /// Object quota: remaining (cur) and maximum (max) allowed objects (names, replies) across all connections
    pub objects: Option<CurMaxPair>,
    // UserUsage provides detailed breakdown of the aggregated numbers.
    // However, dbus-broker exposes usage as real values (not inverted, see CurMaxPair).
}

impl DBusBrokerUserAccounting {
    fn new(uid: u32) -> Self {
        let username = match get_user_by_uid(uid) {
            Some(user) => user.name().to_string_lossy().into_owned(),
            None => uid.to_string(),
        };

        Self {
            uid,
            username,
            ..Default::default()
        }
    }
}

/// D-Bus daemon/broker statistics from org.freedesktop.DBus.Debug.Stats.
/// Works with both dbus-daemon and dbus-broker; broker-specific fields are in separate maps.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct DBusStats {
    /// Current D-Bus message serial number (monotonically increasing message counter)
    pub serial: Option<u32>,
    /// Number of fully authenticated active D-Bus connections
    pub active_connections: Option<u32>,
    /// Number of D-Bus connections still in the authentication handshake phase
    pub incomplete_connections: Option<u32>,
    /// Current number of registered bus names (well-known + unique)
    pub bus_names: Option<u32>,
    /// Peak (high-water mark) number of bus names ever registered simultaneously
    pub peak_bus_names: Option<u32>,
    /// Peak number of bus names registered by a single connection
    pub peak_bus_names_per_connection: Option<u32>,
    /// Current number of active signal match rules across all connections
    pub match_rules: Option<u32>,
    /// Peak number of match rules ever registered simultaneously
    pub peak_match_rules: Option<u32>,
    /// Peak number of match rules registered by a single connection
    pub peak_match_rules_per_connection: Option<u32>,

    /// Per-peer resource accounting (dbus-broker only), keyed by unique connection name
    pub dbus_broker_peer_accounting: Option<HashMap<String, DBusBrokerPeerAccounting>>,
    /// Per-cgroup resource accounting (dbus-broker only), keyed by cgroup name
    pub dbus_broker_cgroup_accounting: Option<HashMap<String, DBusBrokerCGroupAccounting>>,
    /// Per-user resource quota accounting (dbus-broker only), keyed by Unix UID
    pub dbus_broker_user_accounting: Option<HashMap<u32, DBusBrokerUserAccounting>>,
}

impl DBusStats {
    pub fn peer_accounting(&self) -> Option<&HashMap<String, DBusBrokerPeerAccounting>> {
        self.dbus_broker_peer_accounting.as_ref()
    }

    pub fn cgroup_accounting(&self) -> Option<&HashMap<String, DBusBrokerCGroupAccounting>> {
        self.dbus_broker_cgroup_accounting.as_ref()
    }

    pub fn user_accounting(&self) -> Option<&HashMap<u32, DBusBrokerUserAccounting>> {
        self.dbus_broker_user_accounting.as_ref()
    }
}

fn get_u32(dict: &Dict, key: &str) -> Option<u32> {
    let value_key: Value = key.into();
    dict.get(&value_key).ok().and_then(|v| match v.flatten() {
        Some(Value::U32(val)) => Some(*val),
        _ => None,
    })
}

fn get_u32_vec(dict: &Dict, key: &str) -> Option<Vec<u32>> {
    let value_key: Value = key.into();
    dict.get(&value_key).ok().and_then(|v| match v.flatten() {
        Some(Value::Array(array)) => {
            let vec: Vec<u32> = array
                .iter()
                .filter_map(|item| {
                    if let Value::U32(num) = item {
                        Some(*num)
                    } else {
                        None
                    }
                })
                .collect();

            Some(vec)
        }
        _ => None,
    })
}

/* Parse DBusBrokerPeerAccounting from OwnedValue.
 * Expected structure:
 * struct {
 *     string ":1.2197907"
 *     array [
 *         dict entry(
 *              string "UnixUserID"
 *              variant uint32 0
 *         )
 *         ... other fields
 *     ]
 *     array [
 *         dict entry(
 *              string "NameObjects"
 *              uint32 1
 *         )
 *         ... other fields
 *     ]
 * }
 */

fn parse_peer_struct(
    peer_value: &Value,
    well_known_to_peer_names: &HashMap<String, String>,
) -> Option<DBusBrokerPeerAccounting> {
    let peer_struct = match peer_value {
        Value::Structure(peer_struct) => peer_struct,
        _ => return None,
    };

    match peer_struct.fields() {
        [Value::Str(id), Value::Dict(credentials), Value::Dict(stats), ..] => {
            Some(DBusBrokerPeerAccounting {
                id: id.to_string(),
                well_known_name: well_known_to_peer_names.get(id.as_str()).cloned(),
                unix_user_id: get_u32(credentials, "UnixUserID"),
                process_id: get_u32(credentials, "ProcessID"),
                unix_group_ids: get_u32_vec(credentials, "UnixGroupIDs"),
                name_objects: get_u32(stats, "NameObjects"),
                match_bytes: get_u32(stats, "MatchBytes"),
                matches: get_u32(stats, "Matches"),
                reply_objects: get_u32(stats, "ReplyObjects"),
                incoming_bytes: get_u32(stats, "IncomingBytes"),
                incoming_fds: get_u32(stats, "IncomingFds"),
                outgoing_bytes: get_u32(stats, "OutgoingBytes"),
                outgoing_fds: get_u32(stats, "OutgoingFds"),
                activation_request_bytes: get_u32(stats, "ActivationRequestBytes"),
                activation_request_fds: get_u32(stats, "ActivationRequestFds"),
            })
        }
        _ => None,
    }
}

async fn parse_peer_accounting(
    dbus_proxy: &DBusProxy<'_>,
    config: &crate::config::Config,
    owned_value: Option<&OwnedValue>,
) -> Result<Option<Vec<DBusBrokerPeerAccounting>>, MonitordDbusStatsError> {
    // need to keep collecting peer stats when cgroup_stats=true
    // since cgroup_stats is a derivative of peer stats
    if !config.dbus_stats.peer_stats && !config.dbus_stats.cgroup_stats {
        return Ok(None)
    }

    let value: &Value = match owned_value {
        Some(v) => v,
        None => return Ok(None),
    };

    let peers_value = match value {
        Value::Array(peers_value) => peers_value,
        _ => return Ok(None),
    };

    let well_known_to_peer_names = get_well_known_to_peer_names(&dbus_proxy).await?;

    let result = peers_value
        .iter()
        .filter_map(|peer| parse_peer_struct(peer, &well_known_to_peer_names))
        .collect();

    Ok(Some(result))
}

fn filter_and_collect_peer_accounting(
    config: &crate::config::Config,
    peers: Option<&Vec<DBusBrokerPeerAccounting>>,
) -> Option<HashMap<String, DBusBrokerPeerAccounting>> {
    // reject collecting peer stats when told so
    if !config.dbus_stats.peer_stats {
        return None;
    }

    let result = peers?
        .iter()
        .filter(|peer| {
            if config.dbus_stats.peer_well_known_names_only && !peer.has_well_known_name() {
                return false;
            }

            let id = peer.id.to_string();
            let name = peer.get_name();
            if !config.dbus_stats.peer_blocklist.is_empty() {
                if config.dbus_stats.peer_blocklist.contains(&id) ||
                    config.dbus_stats.peer_blocklist.contains(&name) {
                        return false;
                }
            }

            if !config.dbus_stats.peer_allowlist.is_empty() {
                if !config.dbus_stats.peer_allowlist.contains(&id) &&
                    !config.dbus_stats.peer_allowlist.contains(&name) {
                        return false;
                }
            }

            true
        })
        .map(|peer| (peer.id.clone(), peer.clone()))
        .collect();

    Some(result)
}

fn filter_and_collect_cgroup_accounting(
    config: &crate::config::Config,
    peers: Option<&Vec<DBusBrokerPeerAccounting>>,
) -> Option<HashMap<String, DBusBrokerCGroupAccounting>> {
    // reject collecting cgroup stats when told so
    if !config.dbus_stats.cgroup_stats {
        return None;
    }

    let mut result: HashMap<String, DBusBrokerCGroupAccounting> = HashMap::new();

    for peer in peers?.iter() {
        let cgroup_name = match peer.get_cgroup_name() {
            Ok(name) => name,
            Err(err) => {
                error!("Failed to get cgroup name for peer {}: {}", peer.id, err);
                continue;
            }
        };

        if !config.dbus_stats.cgroup_blocklist.is_empty() {
            if config.dbus_stats.cgroup_blocklist.contains(&cgroup_name) {
                continue;
            }
        }

        if !config.dbus_stats.cgroup_allowlist.is_empty() {
            if !config.dbus_stats.cgroup_allowlist.contains(&cgroup_name) {
                continue;
            }
        }

        let entry = result.entry(cgroup_name.clone()).or_insert_with(|| {
            DBusBrokerCGroupAccounting {
                name: cgroup_name,
                ..Default::default()
            }
        });

        entry.combine_with_peer(peer);
    }

    Some(result)
}

/* Parse DBusBrokerUserAccounting from OwnedValue.
 * Expected structure:
 * struct {
 *     uint32 0
 *     array [
 *         struct {
 *             string "Bytes"
 *             uint32 536843240
 *             uint32 536870912
 *         }
 *         ... more fields
 *     ]
 *     # TODO parse usages, ignoring for now
 *     # see src/bus/driver.c:2258
 *     # the part below is not parsed
 *     array [
 *         dict entry(
 *             uint32 0
 *             array [
 *             dict entry(
 *                 string "Bytes"
 *                 uint32 27672
 *             )
 *             ... more fields
 *             ]
 *         )
 *     ]
 * }
 */

fn parse_user_struct(user_value: &Value) -> Option<DBusBrokerUserAccounting> {
    let user_struct = match user_value {
        Value::Structure(user_struct) => user_struct,
        _ => return None,
    };

    match user_struct.fields() {
        [Value::U32(uid), Value::Array(user_stats), ..] => {
            let mut user = DBusBrokerUserAccounting::new(*uid);
            for user_stat in user_stats.iter() {
                if let Value::Structure(user_stat) = user_stat {
                    if let [Value::Str(name), Value::U32(cur), Value::U32(max), ..] =
                        user_stat.fields()
                    {
                        let pair = CurMaxPair {
                            cur: *cur,
                            max: *max,
                        };
                        match name.as_str() {
                            "Bytes" => user.bytes = Some(pair),
                            "Fds" => user.fds = Some(pair),
                            "Matches" => user.matches = Some(pair),
                            "Objects" => user.objects = Some(pair),
                            _ => {} // ignore other fields
                        }
                    }
                }
            }

            Some(user)
        }
        _ => None,
    }
}

fn parse_user_accounting(
    config: &crate::config::Config,
    owned_value: &OwnedValue,
) -> Option<HashMap<u32, DBusBrokerUserAccounting>> {
    // reject collecting user stats when told so
    if !config.dbus_stats.user_stats {
        return None;
    }

    let value: &Value = owned_value;
    let users_value = match value {
        Value::Array(users_value) => users_value,
        _ => return None,
    };

    let result = users_value
        .iter()
        .filter_map(parse_user_struct)
        .filter(|user| {
            let uid = user.uid.to_string();
            if !config.dbus_stats.user_blocklist.is_empty() {
                if config.dbus_stats.user_blocklist.contains(&uid) ||
                    config.dbus_stats.user_blocklist.contains(&user.username) {
                    return false;
                }
            }

            if !config.dbus_stats.user_allowlist.is_empty() {
                if !config.dbus_stats.user_allowlist.contains(&uid) &&
                    !config.dbus_stats.user_allowlist.contains(&user.username) {
                    return false;
                }
            }

            true
        })
        .map(|user| (user.uid, user))
        .collect();

    Some(result)
}

async fn get_well_known_to_peer_names(
    dbus_proxy: &DBusProxy<'_>,
) -> Result<HashMap<String, String>, MonitordDbusStatsError> {
    let dbus_names = dbus_proxy.list_names().await?;
    let mut result = HashMap::new();

    for owned_busname in dbus_names.iter() {
        let name: &BusName = owned_busname;
        if let BusName::WellKnown(_) = name {
            // TODO parallelize
            let owner = dbus_proxy.get_name_owner(name.clone()).await?;
            result.insert(owner.to_string(), name.to_string());
        }
    }

    Ok(result)
}

/// Pull all units from dbus and count how system is setup and behaving
pub async fn parse_dbus_stats(
    config: &crate::config::Config,
    connection: &zbus::Connection,
) -> Result<DBusStats, MonitordDbusStatsError> {
    let dbus_proxy = DBusProxy::new(connection).await?;

    let stats_proxy = StatsProxy::new(connection).await?;
    let stats = stats_proxy.get_stats().await?;
    let peers = parse_peer_accounting(
        &dbus_proxy,
        config,
        stats.rest().get("org.bus1.DBus.Debug.Stats.PeerAccounting"),
    ).await?;

    let dbus_stats = DBusStats {
        serial: stats.serial(),
        active_connections: stats.active_connections(),
        incomplete_connections: stats.incomplete_connections(),
        bus_names: stats.bus_names(),
        peak_bus_names: stats.peak_bus_names(),
        peak_bus_names_per_connection: stats.peak_bus_names_per_connection(),
        match_rules: stats.match_rules(),
        peak_match_rules: stats.peak_match_rules(),
        peak_match_rules_per_connection: stats.peak_match_rules_per_connection(),

        // attempt to parse dbus-broker specific stats
        dbus_broker_peer_accounting: filter_and_collect_peer_accounting(config, peers.as_ref()),
        dbus_broker_cgroup_accounting: filter_and_collect_cgroup_accounting(config, peers.as_ref()),
        dbus_broker_user_accounting: stats
            .rest()
            .get("org.bus1.DBus.Debug.Stats.UserAccounting")
            .map(|user| parse_user_accounting(config, user))
            .unwrap_or_default(),
    };

    Ok(dbus_stats)
}

/// Async wrapper than can update dbus stats when passed a locked struct
pub async fn update_dbus_stats(
    config: Arc<crate::config::Config>,
    connection: zbus::Connection,
    locked_machine_stats: Arc<RwLock<MachineStats>>,
) -> anyhow::Result<()> {
    match parse_dbus_stats(&config, &connection).await {
        Ok(dbus_stats) => {
            let mut machine_stats = locked_machine_stats.write().await;
            machine_stats.dbus_stats = Some(dbus_stats)
        }
        Err(err) => error!("dbus stats failed: {:?}", err),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zvariant::{Array, OwnedValue, Str, Structure, Value};

    #[test]
    fn test_cur_max_pair_usage() {
        let p = CurMaxPair { cur: 10, max: 100 };
        assert_eq!(p.get_usage(), 90);
    }

    #[test]
    fn test_cgroup_accounting_gating_and_skip_errors() {
        let disabled = DBusStats {
            dbus_broker_peer_accounting: Some(HashMap::new()),
            ..Default::default()
        };
        assert!(disabled.cgroup_accounting().is_none());

        let mut peers: HashMap<String, DBusBrokerPeerAccounting> = HashMap::new();
        peers.insert(
            ":1.77".to_string(),
            DBusBrokerPeerAccounting {
                id: ":1.77".to_string(),
                process_id: None,
                ..Default::default()
            },
        );

        let enabled = DBusStats {
            dbus_broker_peer_accounting: Some(peers),
            ..Default::default()
        };

        let cg_map = enabled.cgroup_accounting().expect("map should exist");
        assert!(cg_map.is_empty());
    }

    #[test]
    fn test_combine_with_peer_option_summing() {
        let mut cg = DBusBrokerCGroupAccounting {
            name: "cg1".to_string(),
            name_objects: Some(5),
            match_bytes: None,
            matches: Some(3),
            reply_objects: None,
            incoming_bytes: Some(10),
            incoming_fds: None,
            outgoing_bytes: Some(7),
            outgoing_fds: Some(2),
            activation_request_bytes: None,
            activation_request_fds: Some(1),
        };

        let peer = DBusBrokerPeerAccounting {
            id: ":1.1".to_string(),
            well_known_name: Some("com.example".to_string()),
            unix_user_id: Some(1000),
            process_id: Some(1234),
            unix_group_ids: Some(vec![1000]),
            name_objects: Some(2),
            match_bytes: Some(4),
            matches: None,
            reply_objects: Some(1),
            incoming_bytes: None,
            incoming_fds: Some(5),
            outgoing_bytes: Some(3),
            outgoing_fds: None,
            activation_request_bytes: Some(8),
            activation_request_fds: None,
        };

        cg.combine_with_peer(&peer);

        assert_eq!(cg.name_objects, Some(7));
        assert_eq!(cg.match_bytes, Some(4));
        assert_eq!(cg.matches, Some(3));
        assert_eq!(cg.reply_objects, Some(1));
        assert_eq!(cg.incoming_bytes, Some(10));
        assert_eq!(cg.incoming_fds, Some(5));
        assert_eq!(cg.outgoing_bytes, Some(10));
        assert_eq!(cg.outgoing_fds, Some(2));
        assert_eq!(cg.activation_request_bytes, Some(8));
        assert_eq!(cg.activation_request_fds, Some(1));
    }

    #[test]
    fn test_parse_user_accounting_gating_and_parse() {
        // When user_stats=false, should return None
        let mut cfg = crate::config::Config::default();
        cfg.dbus_stats.user_stats = false;
        let empty_val = Value::Array(Array::from(Vec::<Value>::new()));
        let empty_owned = OwnedValue::try_from(empty_val).expect("owned value conversion");
        assert!(parse_user_accounting(&cfg, &empty_owned).is_none());

        // When user_stats=true, empty array should return Some(empty map)
        cfg.dbus_stats.user_stats = true;
        let empty_val = Value::Array(Array::from(Vec::<Value>::new()));
        let owned = OwnedValue::try_from(empty_val).expect("should convert empty array");
        let parsed = parse_user_accounting(&cfg, &owned).expect("should parse empty");
        assert_eq!(parsed.len(), 0);

        // Non-array input should return None
        let non_array = OwnedValue::try_from(Value::U32(0)).expect("should convert u32 value");
        assert!(parse_user_accounting(&cfg, &non_array).is_none());
    }

    #[test]
    fn test_parse_user_struct_invalid_returns_none() {
        // Build an invalid structure (wrong field types/order) to ensure None is returned
        let invalid = Value::Structure(Structure::from((
            Value::Str(Str::from_static("not_uid")),
            Value::U32(10),
            Value::U32(20),
        )));
        assert!(parse_user_struct(&invalid).is_none());
    }

    #[test]
    fn test_user_metric_name_fallback() {
        // Use a likely-nonexistent uid to force fallback to stringified uid
        let user = DBusBrokerUserAccounting {
            uid: 999_999,
            bytes: Some(CurMaxPair { cur: 5, max: 10 }),
            ..Default::default()
        };
        // If users crate can’t resolve uid, it should fallback to uid string
        assert_eq!(&user.username, "999999");
    }
}
