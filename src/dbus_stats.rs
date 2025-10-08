//! # dbus_stats module
//!
//! Handle getting statistics of our Dbus daemon/broker

use std::collections::HashMap;
use std::fs;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;
use tracing::error;
use users::get_user_by_uid;
use zbus::fdo::{DBusProxy, StatsProxy};
use zbus::names::BusName;
use zvariant::{Dict, OwnedValue, Value};

use crate::MachineStats;

// Unfortunately, various DBus daemons (ex: dbus-broker and dbus-daemon)
// represent stats differently. Moreover, the stats vary across versions of the same daemon.
// Hence, the code uses flexible approach providing max available information.

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct DBusBrokerPeerAccounting {
    pub id: String,
    pub well_known_name: Option<String>,

    // credentials
    pub unix_user_id: Option<u32>,
    pub process_id: Option<u32>,
    pub unix_group_ids: Option<Vec<u32>>,
    // ignoring LinuxSecurityLabel
    // pub linux_security_label: Option<String>,

    // stats
    pub name_objects: Option<u32>,
    pub match_bytes: Option<u32>,
    pub matches: Option<u32>,
    pub reply_objects: Option<u32>,
    pub incoming_bytes: Option<u32>,
    pub incoming_fds: Option<u32>,
    pub outgoing_bytes: Option<u32>,
    pub outgoing_fds: Option<u32>,
    pub activation_request_bytes: Option<u32>,
    pub activation_request_fds: Option<u32>,
}

impl DBusBrokerPeerAccounting {
    pub fn get_name_for_metric(&self) -> String {
        if let Some(ref well_known) = self.well_known_name {
            return well_known.clone();
        }

        let formated_id = self
            .id
            .strip_prefix(':')
            .unwrap_or(&self.id)
            .replace(',', "-");

        if let Some(ref pid) = self.process_id {
            let path = format!("/proc/{}/comm", pid);
            if let Ok(name) = fs::read_to_string(&path) {
                // There might be multiple connections from the same process.
                // As result, need to suffix result with connection_id for uniqueness
                return format!("{}-{}", name.trim(), formated_id);
            }
        }

        formated_id
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct CurMaxPair {
    pub cur: u32,
    pub max: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct DBusBrokerUserAccounting {
    pub uid: u32,

    // stats
    pub bytes: Option<CurMaxPair>,
    pub fds: Option<CurMaxPair>,
    pub matches: Option<CurMaxPair>,
    pub objects: Option<CurMaxPair>,
    // TODO UserUsage
    // see src/util/user.h
}

impl DBusBrokerUserAccounting {
    fn new(uid: u32) -> Self {
        Self {
            uid,
            ..Default::default()
        }
    }

    pub fn get_name_for_metric(&self) -> String {
        match get_user_by_uid(self.uid) {
            Some(user) => user.name().to_string_lossy().into_owned(),
            None => self.uid.to_string(),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct DBusStats {
    pub serial: Option<u32>,
    pub active_connections: Option<u32>,
    pub incomplete_connections: Option<u32>,
    pub bus_names: Option<u32>,
    pub peak_bus_names: Option<u32>,
    pub peak_bus_names_per_connection: Option<u32>,
    pub match_rules: Option<u32>,
    pub peak_match_rules: Option<u32>,
    pub peak_match_rules_per_connection: Option<u32>,

    // dbus-broker specific stats
    pub dbus_broker_peer_accounting: Option<HashMap<String, DBusBrokerPeerAccounting>>,
    pub dbus_broker_user_accounting: Option<HashMap<u32, DBusBrokerUserAccounting>>,
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

fn parse_peer_accounting(
    owned_value: &OwnedValue,
    well_known_to_peer_names: &HashMap<String, String>,
) -> Option<HashMap<String, DBusBrokerPeerAccounting>> {
    let value: &Value = owned_value;
    let peers_value = match value {
        Value::Array(peers_value) => peers_value,
        _ => return None,
    };

    let result = peers_value
        .iter()
        .filter_map(|peer| parse_peer_struct(peer, well_known_to_peer_names))
        .map(|peer| (peer.id.clone(), peer))
        .collect();

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
    owned_value: &OwnedValue,
) -> Option<HashMap<u32, DBusBrokerUserAccounting>> {
    let value: &Value = owned_value;
    let users_value = match value {
        Value::Array(users_value) => users_value,
        _ => return None,
    };

    let result = users_value
        .iter()
        .filter_map(parse_user_struct)
        .map(|user| (user.uid, user))
        .collect();

    Some(result)
}

async fn get_well_known_to_peer_names(
    dbus_proxy: &DBusProxy<'_>,
) -> Result<HashMap<String, String>, Box<dyn std::error::Error + Send + Sync>> {
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
    connection: &zbus::Connection,
) -> Result<DBusStats, Box<dyn std::error::Error + Send + Sync>> {
    let dbus_proxy = DBusProxy::new(connection).await?;
    let well_known_to_peer_names = get_well_known_to_peer_names(&dbus_proxy).await?;

    let stats_proxy = StatsProxy::new(connection).await?;
    let stats = stats_proxy.get_stats().await?;

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
        dbus_broker_peer_accounting: stats
            .rest()
            .get("org.bus1.DBus.Debug.Stats.PeerAccounting")
            .map(|peer| parse_peer_accounting(peer, &well_known_to_peer_names))
            .unwrap_or_default(),
        dbus_broker_user_accounting: stats
            .rest()
            .get("org.bus1.DBus.Debug.Stats.UserAccounting")
            .map(parse_user_accounting)
            .unwrap_or_default(),
    };

    Ok(dbus_stats)
}

/// Async wrapper than can update dbus stats when passed a locked struct
pub async fn update_dbus_stats(
    connection: zbus::Connection,
    locked_machine_stats: Arc<RwLock<MachineStats>>,
) -> anyhow::Result<()> {
    match parse_dbus_stats(&connection).await {
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
    use std::collections::HashMap;
    use zvariant::{Array, Dict, OwnedValue, Signature, StructureBuilder, Value};

    #[test]
    fn test_parse_peer_accounting_returns_map() {
        // Dict signatures
        let sig_s = Signature::from_bytes(b"s").unwrap();
        let sig_v = Signature::from_bytes(b"v").unwrap();
        let sig_u = Signature::from_bytes(b"u").unwrap();

        // Build credentials dict: a{sv}
        let mut credentials = Dict::new(&sig_s, &sig_v);
        credentials
            .add("UnixUserID", OwnedValue::from(1000u32))
            .unwrap();
        credentials
            .add("ProcessID", OwnedValue::from(1234u32))
            .unwrap();
        // UnixGroupIDs as a Value::Array, then to OwnedValue for 'v'
        let gids = Array::from(vec![Value::U32(100), Value::U32(200)]);
        credentials
            .add(
                "UnixGroupIDs",
                OwnedValue::try_from(Value::Array(gids)).unwrap(),
            )
            .unwrap();

        // Build stats dict: a{su}
        let mut stats = Dict::new(&sig_s, &sig_u);
        stats.add("NameObjects", 5u32).unwrap();
        stats.add("Matches", 10u32).unwrap();
        stats.add("IncomingBytes", 1024u32).unwrap();
        stats.add("OutgoingBytes", 2048u32).unwrap();

        // Build the peer structure from Values to match parse_peer_struct pattern:
        let peer_struct = Value::Structure(
            StructureBuilder::new()
                .add_field(Value::Str(":1.123".into()))
                .add_field(Value::Dict(credentials))
                .add_field(Value::Dict(stats))
                .add_field(Value::Str("".into()))
                .build()
                .unwrap(),
        );
        // Let the Array infer element signature from Value::Structure
        let peers_array = Array::from(vec![peer_struct]);

        // Wrap into OwnedValue to pass to parse_peer_accounting
        let owned_peers = OwnedValue::try_from(Value::Array(peers_array)).unwrap();

        // Empty well-known map should not filter anything out
        let well_known_map: HashMap<String, String> = HashMap::new();
        let result = parse_peer_accounting(&owned_peers, &well_known_map);

        assert!(result.is_some(), "parse_peer_accounting returned None");
        let map = result.unwrap();
        eprintln!("Parsed map:\n{:#?}", map); // debug output
        assert_eq!(map.len(), 1, "expected a single peer parsed");
        let peer = map.get(":1.123").expect("peer :1.123 missing");
        assert_eq!(peer.id, ":1.123");
        assert_eq!(peer.well_known_name, None);
        assert_eq!(peer.unix_user_id, Some(1000));
        assert_eq!(peer.process_id, Some(1234));
        assert_eq!(peer.unix_group_ids, Some(vec![100, 200]));
        assert_eq!(peer.name_objects, Some(5));
        assert_eq!(peer.matches, Some(10));
        assert_eq!(peer.incoming_bytes, Some(1024));
        assert_eq!(peer.outgoing_bytes, Some(2048));
    }
}
