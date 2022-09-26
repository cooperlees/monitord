use std::path::PathBuf;
use std::str::FromStr;

use anyhow::Result;
use serde_repr::*;
use strum_macros::EnumString;

#[allow(non_camel_case_types)]
#[derive(Serialize_repr, Deserialize_repr, Debug, Eq, PartialEq, EnumString)]
#[repr(u8)]
pub enum AddressState {
    unknown = 0,
}

#[allow(non_camel_case_types)]
#[derive(Serialize_repr, Deserialize_repr, Debug, Eq, PartialEq, EnumString)]
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

#[derive(Serialize_repr, Deserialize_repr, Debug, Eq, PartialEq, EnumString)]
#[repr(u8)]
pub enum BoolState {
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
#[derive(Serialize_repr, Deserialize_repr, Debug, Eq, PartialEq, EnumString)]
#[repr(u8)]
pub enum CarrierState {
    unknown = 0,
}

#[allow(non_camel_case_types)]
#[derive(Serialize_repr, Deserialize_repr, Debug, Eq, PartialEq, EnumString)]
#[repr(u8)]
pub enum OnlineState {
    unknown = 0,
    online = 1,
}

#[allow(non_camel_case_types)]
#[derive(Serialize_repr, Deserialize_repr, Debug, Eq, PartialEq, EnumString)]
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
    admin_state: AdminState,
    network_file: String,
    oper_state: OperState,
    required_for_online: BoolState,
}

#[derive(Debug, Eq, PartialEq)]
pub struct NetworkdState {
    interface_states: Vec<InterfaceState>,
}

pub const NETWORKD_STATE_FILES: &str = "/run/systemd/netif/links";

/// Parse a networkd state file
pub fn parse_interface_stats(interface_state_str: String) -> Result<InterfaceState, String> {
    let mut interface_state = InterfaceState {
        admin_state: AdminState::unknown,
        network_file: "".to_string(),
        oper_state: OperState::unknown,
        required_for_online: BoolState::False,
    };

    for line in interface_state_str.lines() {
        // Skip comments + lines without =
        if !line.contains('=') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .expect("Unable to split a network state line");
        match key {
            "ADMIN_STATE" => interface_state.admin_state = AdminState::from_str(value).unwrap(),
            "NETWORK_FILE" => interface_state.network_file = value.to_string(),
            "OPER_STATE" => interface_state.oper_state = OperState::from_str(value).unwrap(),
            "REQUIRED_FOR_ONLINE" => {
                interface_state.required_for_online = BoolState::from_str(value).unwrap()
            }
            _ => continue,
        };
    }

    Ok(interface_state)
}

/// Parse interface state files in directory supplied
pub fn parse_interface_state_files(_states_path_path: PathBuf) -> Result<(), String> {
    println!("TBA");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_parse_interface_stats() {
        assert_eq!(
            InterfaceState {
                admin_state: AdminState::configured,
                network_file: "/etc/systemd/network/69-eno4.network".to_string(),
                oper_state: OperState::routable,
                required_for_online: BoolState::True,
            },
            parse_interface_stats(MOCK_INTERFACE_STATE.to_string()).unwrap(),
        );
    }

    #[test]
    fn test_interface_stats_json() {
        let expected_interface_state_json = r###"{"admin_state":4,"network_file":"/etc/systemd/network/69-eno4.network","oper_state":9,"required_for_online":1}"###;
        let stats = parse_interface_stats(MOCK_INTERFACE_STATE.to_string()).unwrap();
        let stats_json = serde_json::to_string(&stats).unwrap();
        assert_eq!(expected_interface_state_json.to_string(), stats_json);
    }

    #[test]
    fn test_parse_interface_state_files() {
        let path = PathBuf::from(NETWORKD_STATE_FILES);
        assert_eq!(Ok(()), parse_interface_state_files(path),)
    }
}
