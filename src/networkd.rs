use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Result;
use dbus::blocking::Connection;
use int_enum::IntEnum;
use serde_repr::*;
use strum_macros::EnumString;
use tracing::error;

/*
systemd enums copied from https://github.com/systemd/systemd/blob/main/src/libsystemd/sd-network/network-util.h
*/

#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr, Deserialize_repr, Clone, Copy, Debug, Eq, PartialEq, EnumString, IntEnum,
)]
#[repr(u8)]
pub enum AddressState {
    unknown = 0,
    off = 1,
    degraded = 2,
    routable = 3,
}

#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr, Deserialize_repr, Clone, Copy, Debug, Eq, PartialEq, EnumString, IntEnum,
)]
#[repr(u8)]
pub enum AdminState {
    unknown = 0,
    pending = 1,
    failed = 2,
    configuring = 3,
    configured = 4,
    unmanaged = 5,
    linger = 6,
}

#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr, Deserialize_repr, Clone, Copy, Debug, Eq, PartialEq, EnumString, IntEnum,
)]
#[repr(u8)]
pub enum BoolState {
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

#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr, Deserialize_repr, Clone, Copy, Debug, Eq, PartialEq, EnumString, IntEnum,
)]
#[repr(u8)]
pub enum CarrierState {
    unknown = 0,
    off = 1,
    #[strum(serialize = "no-carrier", serialize = "no_carrier")]
    no_carrier = 2,
    dormant = 3,
    #[strum(serialize = "degraded-carrier", serialize = "degraded_carrier")]
    degraded_carrier = 4,
    carrier = 5,
    enslaved = 6,
}

#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr, Deserialize_repr, Clone, Copy, Debug, Eq, PartialEq, EnumString, IntEnum,
)]
#[repr(u8)]
pub enum OnlineState {
    unknown = 0,
    offline = 1,
    partial = 2,
    online = 3,
}

#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr, Deserialize_repr, Clone, Copy, Debug, Eq, PartialEq, EnumString, IntEnum,
)]
#[repr(u8)]
pub enum OperState {
    unknown = 0,
    missing = 1,
    off = 2,
    #[strum(serialize = "no-carrier", serialize = "no_carrier")]
    no_carrier = 3,
    dormant = 4,
    #[strum(serialize = "degraded-carrier", serialize = "degraded_carrier")]
    degraded_carrier = 5,
    carrier = 6,
    degraded = 7,
    enslaved = 8,
    routable = 9,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Eq, PartialEq)]
pub struct InterfaceState {
    pub address_state: AddressState,
    pub admin_state: AdminState,
    pub carrier_state: CarrierState,
    pub ipv4_address_state: AddressState,
    pub ipv6_address_state: AddressState,
    pub name: String,
    pub network_file: String,
    pub oper_state: OperState,
    pub required_for_online: BoolState,
}

/// Get interface id + name from dbus list_links API
fn get_interface_links(
    dbus_address: &str,
) -> Result<HashMap<i32, String>, Box<dyn std::error::Error + Send + Sync>> {
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", dbus_address);
    let c = Connection::new_system()?;
    let p = c.with_proxy(
        "org.freedesktop.network1",
        "/org/freedesktop/network1",
        Duration::new(5, 0),
    );
    use crate::dbus::networkd::OrgFreedesktopNetwork1Manager;
    let links = p.list_links()?;
    let mut link_int_to_name: HashMap<i32, String> = HashMap::new();
    for network_link in links {
        link_int_to_name.insert(network_link.0, network_link.1);
    }
    Ok(link_int_to_name)
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Eq, PartialEq)]
pub struct NetworkdState {
    pub interfaces_state: Vec<InterfaceState>,
    pub managed_interfaces: u64,
}

pub const NETWORKD_STATE_FILES: &str = "/run/systemd/netif/links";

/// Parse a networkd state file contents + convert int ID to name via DBUS
pub fn parse_interface_stats(
    interface_state_str: String,
    interface_id: i32,
    interface_id_to_name: &HashMap<i32, String>,
) -> Result<InterfaceState, String> {
    let mut interface_state = InterfaceState {
        address_state: AddressState::unknown,
        admin_state: AdminState::unknown,
        carrier_state: CarrierState::unknown,
        ipv4_address_state: AddressState::unknown,
        ipv6_address_state: AddressState::unknown,
        name: "".to_string(),
        network_file: "".to_string(),
        oper_state: OperState::unknown,
        required_for_online: BoolState::False,
    };

    for line in interface_state_str.lines() {
        // Skip comments + lines without =
        if !line.contains('=') {
            continue;
        }

        // Pull interface name out of list_links generated HashMap
        if interface_id > 0 {
            interface_state.name = interface_id_to_name
                .get(&interface_id)
                .unwrap_or(&String::from(""))
                .to_string();
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
pub fn parse_interface_state_files(
    states_path: PathBuf,
    maybe_network_int_to_name: Option<HashMap<i32, String>>,
    dbus_address: &str,
) -> Result<NetworkdState, std::io::Error> {
    let mut managed_interface_count: u64 = 0;
    let mut interfaces_state = vec![];

    let network_int_to_name = match maybe_network_int_to_name {
        None => match get_interface_links(dbus_address) {
            Ok(hashmap) => hashmap,
            Err(err) => {
                panic!("Unable to get interface links via DBUS: {:#?}", err)
            }
        },
        Some(valid_hashmap) => valid_hashmap,
    };

    for state_file_dir in fs::read_dir(&states_path)? {
        let state_file = match state_file_dir {
            Ok(sf) => sf,
            Err(err) => {
                error!("Unable to read dir {:?}: {}", states_path.as_os_str(), err);
                break;
            }
        };
        if !state_file.path().is_file() {
            continue;
        }
        let interface_stats_file_str =
            fs::read_to_string(state_file.path()).expect("Unable to read networkd state file");
        if !interface_stats_file_str.contains("NETWORK_FILE") {
            continue;
        }
        managed_interface_count += 1;
        let fname = state_file.file_name();
        let interface_id: i32 = i32::from_str(fname.to_str().unwrap_or("0")).unwrap_or(0);
        match parse_interface_stats(interface_stats_file_str, interface_id, &network_int_to_name) {
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
IPV4_ADDRESS_STATE=routable
IPV6_ADDRESS_STATE=degraded
ONLINE_STATE=online
REQUIRED_FOR_ONLINE=yes
REQUIRED_OPER_STATE_FOR_ONLINE=degraded
REQUIRED_FAMILY_FOR_ONLINE=any
ACTIVATION_POLICY=up
NETWORK_FILE=/etc/systemd/network/69-eno4.network
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
            ipv4_address_state: AddressState::routable,
            ipv6_address_state: AddressState::degraded,
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
                MOCK_INTERFACE_STATE.to_string(),
                2,
                &return_mock_int_name_hashmap().unwrap()
            )
            .unwrap(),
        );
    }

    #[test]
    fn test_parse_interface_stats_json() {
        // 'name' stays as an empty string cause we don't pass in networkctl json or an interface id
        let expected_interface_state_json = r###"{"address_state":3,"admin_state":4,"carrier_state":5,"ipv4_address_state":3,"ipv6_address_state":2,"name":"","network_file":"/etc/systemd/network/69-eno4.network","oper_state":9,"required_for_online":1}"###;
        let stats =
            parse_interface_stats(MOCK_INTERFACE_STATE.to_string(), 0, &HashMap::new()).unwrap();
        let stats_json = serde_json::to_string(&stats).unwrap();
        assert_eq!(expected_interface_state_json.to_string(), stats_json);
    }

    #[test]
    fn test_parse_interface_state_files() -> Result<()> {
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
                path,
                return_mock_int_name_hashmap(),
                crate::DEFAULT_DBUS_ADDRESS
            )
            .unwrap()
        );
        Ok(())
    }

    #[test]
    fn test_parse_interface_state_files_json() -> Result<()> {
        let expected_interface_state_json = r###"{"interfaces_state":[{"address_state":3,"admin_state":4,"carrier_state":5,"ipv4_address_state":3,"ipv6_address_state":2,"name":"eth69","network_file":"/etc/systemd/network/69-eno4.network","oper_state":9,"required_for_online":1}],"managed_interfaces":1}"###;

        let temp_dir = tempdir()?;
        // As the networkctl JSON has no interface with index 69 name gets no value ...
        let file_path = temp_dir.path().join("69");
        let mut state_file = File::create(file_path)?;
        writeln!(state_file, "{}", MOCK_INTERFACE_STATE)?;

        let path = PathBuf::from(temp_dir.path());
        let interface_stats = parse_interface_state_files(
            path,
            return_mock_int_name_hashmap(),
            crate::DEFAULT_DBUS_ADDRESS,
        )
        .unwrap();
        let interface_stats_json = serde_json::to_string(&interface_stats).unwrap();
        assert_eq!(
            expected_interface_state_json.to_string(),
            interface_stats_json
        );
        Ok(())
    }

    #[test]
    fn test_enums_to_ints() -> Result<()> {
        assert_eq!(3, AddressState::routable as u64,);
        let carrier_state_int: i64 = CarrierState::degraded_carrier.int_value().into();
        assert_eq!(4, carrier_state_int);
        assert_eq!(1, BoolState::True as i64,);
        let bool_state_false_int: u8 = BoolState::False.int_value().into();
        assert_eq!(0, bool_state_false_int);

        Ok(())
    }
}
