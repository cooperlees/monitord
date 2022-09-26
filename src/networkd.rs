use std::path::PathBuf;
use std::str::FromStr;

use anyhow::Result;
use strum_macros::EnumString;

#[allow(non_camel_case_types)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Eq, PartialEq, EnumString)]
pub enum AddressState {
    unknown,
}

#[allow(non_camel_case_types)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Eq, PartialEq, EnumString)]
pub enum AdminState {
    unknown,
    pending,
    failed,
    configuring,
    configured,
    unmanaged,
    linger,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Eq, PartialEq, EnumString)]
pub enum BoolState {
    #[strum(serialize = "false", serialize = "False")]
    False,
    #[strum(serialize = "true", serialize = "True")]
    True,
}

#[allow(non_camel_case_types)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Eq, PartialEq, EnumString)]
pub enum CarrierState {
    unknown,
}

#[allow(non_camel_case_types)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Eq, PartialEq, EnumString)]
pub enum OnlineState {
    unknown,
    online,
}

#[allow(non_camel_case_types)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Eq, PartialEq, EnumString)]
pub enum OperState {
    unknown,
    missing,
    off,
    no_carrier,
    dormant,
    degraded_carrier,
    carrier,
    degraded,
    enslaved,
    routable,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Eq, PartialEq)]
pub struct InterfaceState {
    admin_state: AdminState,
    network_file: String,
    oper_state: OperState,
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
            },
            parse_interface_stats(MOCK_INTERFACE_STATE.to_string()).unwrap(),
        );
    }

    // TODO: Change enum values into ints
    #[test]
    fn test_interface_stats_json() {
        let expected_interface_state_json= r###"{"admin_state":"configured","network_file":"/etc/systemd/network/69-eno4.network","oper_state":"routable"}"###;
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
