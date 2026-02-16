//! # networkd module
//!
//! All structs, enums and methods specific to systemd-networkd.
//! Enumerations were copied from <https://github.com/systemd/systemd/blob/main/src/libsystemd/sd-network/network-util.h>

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::Result;
use int_enum::IntEnum;
use serde_repr::*;
use strum_macros::EnumIter;
use strum_macros::EnumString;
use tokio::sync::RwLock;
use tracing::error;

use crate::MachineStats;

/// Address configuration state of a networkd-managed interface.
/// Ref: <https://github.com/systemd/systemd/blob/main/src/libsystemd/sd-network/network-util.h>
#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr,
    Deserialize_repr,
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    EnumIter,
    EnumString,
    IntEnum,
    strum_macros::Display,
)]
#[repr(u8)]
pub enum AddressState {
    /// Address state could not be determined
    #[default]
    unknown = 0,
    /// No addresses are configured on this interface
    off = 1,
    /// Addresses are configured but none provide full connectivity (e.g. link-local only)
    degraded = 2,
    /// At least one globally routable address is configured
    routable = 3,
}

/// Administrative state of a networkd-managed interface (networkd's own management lifecycle).
/// Ref: <https://github.com/systemd/systemd/blob/main/src/libsystemd/sd-network/network-util.h>
#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr,
    Deserialize_repr,
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    EnumIter,
    EnumString,
    IntEnum,
    strum_macros::Display,
)]
#[repr(u8)]
pub enum AdminState {
    /// Administrative state could not be determined
    #[default]
    unknown = 0,
    /// Interface is pending configuration by networkd
    pending = 1,
    /// networkd failed to configure this interface
    failed = 2,
    /// Interface is currently being configured by networkd
    configuring = 3,
    /// Interface has been successfully configured by networkd
    configured = 4,
    /// Interface is not managed by networkd
    unmanaged = 5,
    /// Interface is lingering (was managed but its .network file was removed)
    linger = 6,
}

/// Enumeration of a true (yes) / false (no) options - e.g. required for online
#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr,
    Deserialize_repr,
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    EnumIter,
    EnumString,
    IntEnum,
    strum_macros::Display,
)]
#[repr(u8)]
pub enum BoolState {
    #[default]
    unknown = u8::MAX,
    #[strum(
        serialize = "false",
        serialize = "False",
        serialize = "no",
        serialize = "No"
    )]
    False = 0,
    #[strum(
        serialize = "true",
        serialize = "True",
        serialize = "yes",
        serialize = "Yes"
    )]
    True = 1,
}

/// Physical carrier (link layer) state of a networkd-managed interface.
/// Ref: <https://github.com/systemd/systemd/blob/main/src/libsystemd/sd-network/network-util.h>
#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr,
    Deserialize_repr,
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    EnumIter,
    EnumString,
    IntEnum,
    strum_macros::Display,
)]
#[repr(u8)]
pub enum CarrierState {
    /// Carrier state could not be determined
    #[default]
    unknown = 0,
    /// Interface is administratively down (IFF_UP not set)
    off = 1,
    /// Interface is up but no carrier signal detected (cable unplugged or no link partner)
    #[strum(serialize = "no-carrier", serialize = "no_carrier")]
    no_carrier = 2,
    /// Carrier detected but interface is in a dormant/standby state
    dormant = 3,
    /// Carrier detected but in a degraded condition
    #[strum(serialize = "degraded-carrier", serialize = "degraded_carrier")]
    degraded_carrier = 4,
    /// Full carrier signal present and link is operational
    carrier = 5,
    /// Interface is enslaved to a bond/bridge master
    enslaved = 6,
}

/// Overall online state of the system as determined by systemd-networkd-wait-online logic.
/// Ref: <https://github.com/systemd/systemd/blob/main/src/libsystemd/sd-network/network-util.h>
#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr,
    Deserialize_repr,
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    EnumIter,
    EnumString,
    IntEnum,
    strum_macros::Display,
)]
#[repr(u8)]
pub enum OnlineState {
    /// Online state could not be determined
    #[default]
    unknown = 0,
    /// No required interfaces are online
    offline = 1,
    /// Some required interfaces are online but not all
    partial = 2,
    /// All required interfaces are online
    online = 3,
}

/// Operational state of a networkd-managed interface combining carrier and address information.
/// Ref: <https://github.com/systemd/systemd/blob/main/src/libsystemd/sd-network/network-util.h>
#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr,
    Deserialize_repr,
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    EnumIter,
    EnumString,
    IntEnum,
    strum_macros::Display,
)]
#[repr(u8)]
pub enum OperState {
    /// Operational state could not be determined
    #[default]
    unknown = 0,
    /// Interface is missing from the system
    missing = 1,
    /// Interface is administratively down
    off = 2,
    /// Interface is up but has no carrier signal
    #[strum(serialize = "no-carrier", serialize = "no_carrier")]
    no_carrier = 3,
    /// Interface has carrier but is in a dormant/standby state
    dormant = 4,
    /// Interface carrier is in a degraded condition
    #[strum(serialize = "degraded-carrier", serialize = "degraded_carrier")]
    degraded_carrier = 5,
    /// Interface has carrier but no addresses configured
    carrier = 6,
    /// Interface is operational but only has link-local or non-routable addresses
    degraded = 7,
    /// Interface is enslaved to a bond/bridge master
    enslaved = 8,
    /// Interface is fully operational with at least one routable address
    routable = 9,
}

/// Per-interface state collected from systemd-networkd state files in /run/systemd/netif/links/
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct InterfaceState {
    /// Combined address state across all address families (IPv4 + IPv6)
    pub address_state: AddressState,
    /// networkd administrative state (whether networkd has finished configuring this interface)
    pub admin_state: AdminState,
    /// Physical carrier (link layer) state of the interface
    pub carrier_state: CarrierState,
    /// IPv4-specific address state (off, degraded, or routable)
    pub ipv4_address_state: AddressState,
    /// IPv6-specific address state (off, degraded, or routable)
    pub ipv6_address_state: AddressState,
    /// Interface name as reported by the kernel (e.g. "eth0", "enp3s0")
    pub name: String,
    /// Path to the .network configuration file applied to this interface
    pub network_file: String,
    /// Operational state combining carrier detection and address configuration
    pub oper_state: OperState,
    /// Whether this interface is required for the system to be considered online
    pub required_for_online: BoolState,
}

/// Get interface id + name from dbus list_links API
async fn get_interface_links(
    connection: &zbus::Connection,
) -> Result<HashMap<i32, String>, Box<dyn std::error::Error + Send + Sync>> {
    let p = crate::dbus::zbus_networkd::ManagerProxy::new(connection).await?;
    let links = p.list_links().await?;
    let mut link_int_to_name: HashMap<i32, String> = HashMap::new();
    for network_link in links {
        link_int_to_name.insert(network_link.0, network_link.1);
    }
    Ok(link_int_to_name)
}

/// Aggregated systemd-networkd state: per-interface details and total managed interface count
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct NetworkdState {
    /// State details for each networkd-managed interface
    pub interfaces_state: Vec<InterfaceState>,
    /// Total number of interfaces managed by networkd (those with a NETWORK_FILE entry)
    pub managed_interfaces: u64,
}

pub const NETWORKD_STATE_FILES: &str = "/run/systemd/netif/links";

/// Parse a networkd state file contents + convert int ID to name via DBUS
pub fn parse_interface_stats(
    interface_state_str: &str,
    interface_id: i32,
    interface_id_to_name: &HashMap<i32, String>,
) -> Result<InterfaceState, String> {
    let mut interface_state = InterfaceState::default();

    // Pull interface name out of list_links generated HashMap (once, not per line)
    if interface_id > 0 {
        if let Some(name) = interface_id_to_name.get(&interface_id) {
            interface_state.name = name.clone();
        }
    }

    for line in interface_state_str.lines() {
        // Skip comments + lines without =
        if !line.contains('=') {
            continue;
        }

        let (key, value) = line
            .split_once('=')
            .expect("Unable to split a network state line");
        match key {
            "ADDRESS_STATE" => {
                interface_state.address_state =
                    AddressState::from_str(value).unwrap_or(AddressState::unknown)
            }
            "ADMIN_STATE" => {
                interface_state.admin_state =
                    AdminState::from_str(value).unwrap_or(AdminState::unknown)
            }
            "CARRIER_STATE" => {
                interface_state.carrier_state =
                    CarrierState::from_str(value).unwrap_or(CarrierState::unknown)
            }
            "IPV4_ADDRESS_STATE" => {
                interface_state.ipv4_address_state =
                    AddressState::from_str(value).unwrap_or(AddressState::unknown)
            }
            "IPV6_ADDRESS_STATE" => {
                interface_state.ipv6_address_state =
                    AddressState::from_str(value).unwrap_or(AddressState::unknown)
            }
            "NETWORK_FILE" => interface_state.network_file = value.to_string(),
            "OPER_STATE" => {
                interface_state.oper_state =
                    OperState::from_str(value).unwrap_or(OperState::unknown)
            }
            "REQUIRED_FOR_ONLINE" => {
                interface_state.required_for_online =
                    BoolState::from_str(value).unwrap_or(BoolState::unknown)
            }
            _ => continue,
        };
    }

    Ok(interface_state)
}

/// Parse interface state files in directory supplied
pub async fn parse_interface_state_files(
    states_path: &PathBuf,
    maybe_network_int_to_name: Option<HashMap<i32, String>>,
    maybe_connection: Option<&zbus::Connection>,
) -> Result<NetworkdState, std::io::Error> {
    let mut managed_interface_count: u64 = 0;
    let mut interfaces_state = vec![];

    let network_int_to_name = match maybe_network_int_to_name {
        None => {
            if let Some(connection) = maybe_connection {
                match get_interface_links(connection).await {
                    Ok(hashmap) => hashmap,
                    Err(err) => {
                        error!(
                            "Unable to get interface links via DBUS - is networkd running?: {:#?}",
                            err
                        );
                        return Ok(NetworkdState::default());
                    }
                }
            } else {
                error!(
                    "Unable to get interface links via DBUS and no network_int_to_name supplied"
                );
                return Ok(NetworkdState::default());
            }
        }
        Some(valid_hashmap) => valid_hashmap,
    };

    let mut state_file_dir_entries = tokio::fs::read_dir(states_path).await?;
    while let Some(state_file) = state_file_dir_entries.next_entry().await? {
        if !state_file.path().is_file() {
            continue;
        }
        let interface_stats_file_str = tokio::fs::read_to_string(state_file.path()).await?;
        if !interface_stats_file_str.contains("NETWORK_FILE") {
            continue;
        }
        managed_interface_count += 1;
        let fname = state_file.file_name();
        let interface_id: i32 = i32::from_str(fname.to_str().unwrap_or("0")).unwrap_or(0);
        match parse_interface_stats(
            &interface_stats_file_str,
            interface_id,
            &network_int_to_name,
        ) {
            Ok(interface_state) => interfaces_state.push(interface_state),
            Err(err) => error!(
                "Unable to parse interface statistics for {:?}: {}",
                state_file.path().into_os_string(),
                err
            ),
        }
    }
    Ok(NetworkdState {
        interfaces_state,
        managed_interfaces: managed_interface_count,
    })
}

/// Async wrapper than can update networkd stats when passed a locked struct
pub async fn update_networkd_stats(
    states_path: PathBuf,
    maybe_network_int_to_name: Option<HashMap<i32, String>>,
    connection: zbus::Connection,
    locked_machine_stats: Arc<RwLock<MachineStats>>,
) -> anyhow::Result<()> {
    match parse_interface_state_files(&states_path, maybe_network_int_to_name, Some(&connection))
        .await
    {
        Ok(networkd_stats) => {
            let mut machine_stats = locked_machine_stats.write().await;
            machine_stats.networkd = networkd_stats;
        }
        Err(err) => error!("networkd stats failed: {:?}", err),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    const MOCK_INTERFACE_STATE: &str = r###"# This is private data. Do not parse.
ADMIN_STATE=configured
OPER_STATE=routable
CARRIER_STATE=carrier
ADDRESS_STATE=routable
IPV4_ADDRESS_STATE=degraded
IPV6_ADDRESS_STATE=routable
ONLINE_STATE=online
REQUIRED_FOR_ONLINE=yes
REQUIRED_OPER_STATE_FOR_ONLINE=degraded:routable
REQUIRED_FAMILY_FOR_ONLINE=any
ACTIVATION_POLICY=up
NETWORK_FILE=/etc/systemd/network/69-eno4.network
NETWORK_FILE_DROPINS=""
DNS=8.8.8.8 8.8.4.4
NTP=
SIP=
DOMAINS=
ROUTE_DOMAINS=
LLMNR=yes
MDNS=no
"###;

    fn return_expected_interface_state() -> InterfaceState {
        InterfaceState {
            address_state: AddressState::routable,
            admin_state: AdminState::configured,
            carrier_state: CarrierState::carrier,
            ipv4_address_state: AddressState::degraded,
            ipv6_address_state: AddressState::routable,
            name: "eth0".to_string(),
            network_file: "/etc/systemd/network/69-eno4.network".to_string(),
            oper_state: OperState::routable,
            required_for_online: BoolState::True,
        }
    }

    fn return_mock_int_name_hashmap() -> Option<HashMap<i32, String>> {
        let mut h: HashMap<i32, String> = HashMap::new();
        h.insert(2, String::from("eth0"));
        h.insert(69, String::from("eth69"));
        Some(h)
    }

    #[test]
    fn test_parse_interface_stats() {
        assert_eq!(
            return_expected_interface_state(),
            parse_interface_stats(
                MOCK_INTERFACE_STATE,
                2,
                &return_mock_int_name_hashmap().expect("Failed to get a mock int name hashmap"),
            )
            .expect("Failed to parse interface stats"),
        );
    }

    #[test]
    fn test_parse_interface_stats_json() {
        // 'name' stays as an empty string cause we don't pass in networkctl json or an interface id
        let expected_interface_state_json = r###"{"address_state":3,"admin_state":4,"carrier_state":5,"ipv4_address_state":2,"ipv6_address_state":3,"name":"","network_file":"/etc/systemd/network/69-eno4.network","oper_state":9,"required_for_online":1}"###;
        let stats = parse_interface_stats(MOCK_INTERFACE_STATE, 0, &HashMap::new()).unwrap();
        let stats_json = serde_json::to_string(&stats).unwrap();
        assert_eq!(expected_interface_state_json.to_string(), stats_json);
    }

    #[tokio::test]
    async fn test_parse_interface_state_files() -> Result<()> {
        let expected_files = NetworkdState {
            interfaces_state: vec![return_expected_interface_state()],
            managed_interfaces: 1,
        };

        let temp_dir = tempdir()?;
        // Filename of '2' is important as it needs to correspond to the interface id / index
        let file_path = temp_dir.path().join("2");
        let mut state_file = File::create(file_path)?;
        writeln!(state_file, "{}", MOCK_INTERFACE_STATE)?;

        let path = PathBuf::from(temp_dir.path());
        assert_eq!(
            expected_files,
            parse_interface_state_files(
                &path,
                return_mock_int_name_hashmap(),
                None, // No DBUS in tests
            )
            .await
            .expect("Problem with parsing interface stte files")
        );
        Ok(())
    }

    #[test]
    fn test_enums_to_ints() -> Result<()> {
        assert_eq!(3, AddressState::routable as u64);
        let carrier_state_int: u8 = u8::from(CarrierState::degraded_carrier);
        assert_eq!(4, carrier_state_int);
        assert_eq!(1, BoolState::True as i64);
        let bool_state_false_int: u8 = u8::from(BoolState::False);
        assert_eq!(0, bool_state_false_int);

        Ok(())
    }
}
