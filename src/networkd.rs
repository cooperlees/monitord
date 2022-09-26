#![allow(dead_code)]
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::Result;
use strum_macros::EnumString;

const NETWORKD_STATE_FILES: &str = "/run/systemd/netif/links";

#[allow(non_camel_case_types)]
#[derive(Debug, Eq, PartialEq)]
pub enum AddressState {
    unknown,
}

#[allow(non_camel_case_types)]
#[derive(Debug, Eq, PartialEq, EnumString)]
pub enum AdminState {
    unknown,
    pending,
    failed,
    configuring,
    configured,
    unmanaged,
    linger,
}

#[derive(Debug, Eq, PartialEq)]
pub enum BoolState {
    False,
    True,
}

#[allow(non_camel_case_types)]
#[derive(Debug, Eq, PartialEq)]
pub enum CarrierState {
    unknown,
}

#[allow(non_camel_case_types)]
#[derive(Debug, Eq, PartialEq)]
pub enum OnlineState {
    unknown,
    online,
}

#[allow(non_camel_case_types)]
#[derive(Debug, Eq, PartialEq)]
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

#[derive(Debug, Eq, PartialEq)]
pub struct InterfaceState {
    admin_state: AdminState,
    network_file: String,
}

#[derive(Debug, Eq, PartialEq)]
pub struct NetworkdState {
    interface_states: Vec<InterfaceState>,
}

/// Parse a networkd state file
pub fn parse_interface_stats(interface_state_str: String) -> Result<InterfaceState, String> {
    let mut interface_state = InterfaceState {
        admin_state: AdminState::unknown,
        network_file: "".to_string(),
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
            _ => continue,
        };
    }

    Ok(interface_state)
}

/// Parse interface state files in directory supplied
pub fn get_interface_state_files(_states_path_path: PathBuf) -> Result<(), String> {
    println!("TBA");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_interface_stats() {
        let interface_state = r###"# This is private data. Do not parse.
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

        assert_eq!(
            parse_interface_stats(interface_state.to_string()).unwrap(),
            InterfaceState {
                admin_state: AdminState::configured,
                network_file: "/etc/systemd/network/69-eno4.network".to_string()
            },
        );
    }
}
