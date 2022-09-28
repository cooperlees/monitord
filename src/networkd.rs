use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::str::FromStr;

use anyhow::Result;
use log::error;
use serde_repr::*;
use strum_macros::EnumString;

/*
systemd enums copied from https://github.com/systemd/systemd/blob/main/src/libsystemd/sd-network/network-util.h
*/

#[allow(non_camel_case_types)]
#[derive(Serialize_repr, Deserialize_repr, Debug, Eq, PartialEq, EnumString)]
#[repr(u8)]
pub enum AddressState {
    unknown = 0,
    off = 1,
    degraded = 2,
    routable = 3,
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
#[derive(Serialize_repr, Deserialize_repr, Debug, Eq, PartialEq, EnumString)]
#[repr(u8)]
pub enum OnlineState {
    unknown = 0,
    offline = 1,
    partial = 2,
    online = 3,
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

/// Take an interface id and return the name or empty string
fn interface_id_to_name(id: u64, networkctl_json: &serde_json::Value) -> String {
    let interfaces_array = networkctl_json["Interfaces"].as_array().unwrap();
    for interface in interfaces_array.iter() {
        let interface_index: u64 = interface["Index"].as_u64().unwrap();
        if interface_index == id {
            return interface["Name"].as_str().unwrap().to_string();
        }
    }
    error!("Unable to find interface name for id {}", id);
    "".to_string()
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Eq, PartialEq)]
pub struct NetworkdState {
    pub interfaces_state: Vec<InterfaceState>,
    pub managed_interfaces: u64,
}

pub const NETWORKCTL_BINARY: &str = "/usr/bin/networkctl";
pub const NETWORKD_STATE_FILES: &str = "/run/systemd/netif/links";

/// Parse a networkd state file contents
pub fn parse_interface_stats(
    interface_state_str: String,
    interface_id: u64,
    networkctl_json: Option<&serde_json::Value>,
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

        // The double ifs are due to a rust bug: #53667 <https://github.com/rust-lang/rust/issues/53667
        if let Some(actual_networkctl_json) = networkctl_json {
            if interface_id > 0 {
                interface_state.name = interface_id_to_name(interface_id, actual_networkctl_json);
            }
        }

        let (key, value) = line
            .split_once('=')
            .expect("Unable to split a network state line");
        match key {
            "ADDRESS_STATE" => {
                interface_state.address_state = AddressState::from_str(value).unwrap()
            }
            "ADMIN_STATE" => interface_state.admin_state = AdminState::from_str(value).unwrap(),
            "CARRIER_STATE" => {
                interface_state.carrier_state = CarrierState::from_str(value).unwrap()
            }
            "IPV4_ADDRESS_STATE" => {
                interface_state.ipv4_address_state = AddressState::from_str(value).unwrap()
            }
            "IPV6_ADDRESS_STATE" => {
                interface_state.ipv6_address_state = AddressState::from_str(value).unwrap()
            }
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
pub fn parse_interface_state_files(
    states_path_path: PathBuf,
    networkctl_binary: &str,
    args: Vec<String>,
) -> Result<NetworkdState, std::io::Error> {
    let mut managed_interface_count: u64 = 0;
    let mut interfaces_state = vec![];

    let networkctl_json = parse_networkctl_list(networkctl_binary, args).unwrap();
    for state_file_dir in fs::read_dir(&states_path_path)? {
        let state_file = state_file_dir.unwrap();
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
        let interface_id: u64 = u64::from_str(fname.to_str().unwrap_or("0")).unwrap_or(0);
        interfaces_state.push(
            parse_interface_stats(
                interface_stats_file_str,
                interface_id,
                Some(&networkctl_json),
            )
            .unwrap(),
        );
    }
    Ok(NetworkdState {
        interfaces_state,
        managed_interfaces: managed_interface_count,
    })
}

pub fn parse_networkctl_list(
    network_ctl_binary: &str,
    args: Vec<String>,
) -> Result<serde_json::Value, serde_json::Error> {
    let err_msg = format!(
        "failed to execute '{} {}'",
        network_ctl_binary,
        args.join(" ")
    );
    let output = Command::new(network_ctl_binary)
        .args(&args)
        .output()
        .expect(&err_msg);
    if !output.status.success() {
        error!(
            "Failed to obtain '{} {}' JSON output",
            network_ctl_binary,
            args.join(" ")
        );
        let v: serde_json::Value = serde_json::from_str("{}")?;
        return Ok(v);
    }
    let stdout = String::from_utf8(output.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout)?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    const ECHO_BINARY: &str = "/usr/bin/echo";
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

    const NETWORKCTL_JSON: &str = r###"{"Interfaces":[{"Index":1,"Name":"lo","Type":"loopback","Flags":65609,"FlagsString":"up,loopback,running,lower-up","KernelOperationalState":0,"KernelOperationalStateString":"unknown","MTU":65536,"MinimumMTU":0,"MaximumMTU":4294967295,"AdministrativeState":"unmanaged","OperationalState":"carrier","CarrierState":"carrier","AddressState":"off","IPv4AddressState":"off","IPv6AddressState":"off","OnlineState":null,"Addresses":[{"Family":10,"Address":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,1],"PrefixLength":128,"Scope":254,"ScopeString":"host","Flags":128,"FlagsString":"permanent","ConfigSource":"foreign","ConfigState":"configured"},{"Family":2,"Address":[127,0,0,1],"PrefixLength":8,"Scope":254,"ScopeString":"host","Flags":128,"FlagsString":"permanent","ConfigSource":"foreign","ConfigState":"configured"}],"Routes":[{"Family":2,"Destination":[127,0,0,0],"DestinationPrefixLength":32,"PreferredSource":[127,0,0,1],"Scope":253,"ScopeString":"link","Protocol":2,"ProtocolString":"kernel","Type":3,"TypeString":"broadcast","Priority":0,"Table":255,"TableString":"local(255)","Preference":0,"Flags":0,"FlagsString":"","ConfigSource":"foreign","ConfigState":"configured"},{"Family":2,"Destination":[127,0,0,1],"DestinationPrefixLength":32,"PreferredSource":[127,0,0,1],"Scope":254,"ScopeString":"host","Protocol":2,"ProtocolString":"kernel","Type":2,"TypeString":"local","Priority":0,"Table":255,"TableString":"local(255)","Preference":0,"Flags":0,"FlagsString":"","ConfigSource":"foreign","ConfigState":"configured"},{"Family":2,"Destination":[127,0,0,0],"DestinationPrefixLength":8,"PreferredSource":[127,0,0,1],"Scope":254,"ScopeString":"host","Protocol":2,"ProtocolString":"kernel","Type":2,"TypeString":"local","Priority":0,"Table":255,"TableString":"local(255)","Preference":0,"Flags":0,"FlagsString":"","ConfigSource":"foreign","ConfigState":"configured"},{"Family":2,"Destination":[127,255,255,255],"DestinationPrefixLength":32,"PreferredSource":[127,0,0,1],"Scope":253,"ScopeString":"link","Protocol":2,"ProtocolString":"kernel","Type":3,"TypeString":"broadcast","Priority":0,"Table":255,"TableString":"local(255)","Preference":0,"Flags":0,"FlagsString":"","ConfigSource":"foreign","ConfigState":"configured"},{"Family":10,"Destination":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,1],"DestinationPrefixLength":128,"Scope":0,"ScopeString":"global","Protocol":2,"ProtocolString":"kernel","Type":2,"TypeString":"local","Priority":0,"Table":255,"TableString":"local(255)","Preference":0,"Flags":0,"FlagsString":"","ConfigSource":"foreign","ConfigState":"configured"},{"Family":10,"Destination":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,1],"DestinationPrefixLength":128,"Scope":0,"ScopeString":"global","Protocol":2,"ProtocolString":"kernel","Type":1,"TypeString":"unicast","Priority":256,"Table":254,"TableString":"main(254)","Preference":0,"Flags":0,"FlagsString":"","ConfigSource":"foreign","ConfigState":"configured"}]},{"Index":2,"Name":"eth0","Type":"ether","Driver":"bnxt_en","Flags":69699,"FlagsString":"up,broadcast,running,multicast,lower-up","KernelOperationalState":6,"KernelOperationalStateString":"up","MTU":1500,"MinimumMTU":60,"MaximumMTU":9500,"HardwareAddress":[188,151,225,137,250,44],"PermanentHardwareAddress":[188,151,225,137,250,44],"BroadcastAddress":[255,255,255,255,255,255],"IPv6LinkLocalAddress":[254,128,0,0,0,0,0,0,190,151,225,255,254,137,250,44],"AdministrativeState":"configured","OperationalState":"routable","CarrierState":"carrier","AddressState":"routable","IPv4AddressState":"off","IPv6AddressState":"routable","OnlineState":"online","NetworkFile":"/usr/lib/systemd/network/00-metalos-eth0.network","RequiredForOnline":true,"RequiredOperationalStateForOnline":["degraded","routable"],"RequiredFamilyForOnline":"any","ActivationPolicy":"up","LinkFile":"/usr/lib/systemd/network/00-metalos-eth0.link","Path":"pci-0000:02:00.0","Vendor":"Broadcom Inc. and subsidiaries","Model":"BCM57452 NetXtreme-E 10Gb/25Gb/40Gb/50Gb Ethernet","SearchDomains":[{"Domain":"27.lla2.facebook.com","ConfigSource":"static"},{"Domain":"lla2.facebook.com","ConfigSource":"static"},{"Domain":"facebook.com","ConfigSource":"static"},{"Domain":"tfbnw.net","ConfigSource":"static"}],"DNSSettings":[{"LLMNR":"yes","ConfigSource":"static"},{"MDNS":"no","ConfigSource":"static"}],"Addresses":[{"Family":10,"Address":[254,128,0,0,0,0,0,0,190,151,225,255,254,137,250,44],"PrefixLength":64,"Scope":253,"ScopeString":"link","Flags":128,"FlagsString":"permanent","ConfigSource":"foreign","ConfigState":"configured"},{"Family":10,"Address":[36,1,219,0,48,44,65,36,250,206,0,0,2,34,0,0],"PrefixLength":64,"Scope":0,"ScopeString":"global","Flags":128,"FlagsString":"permanent","ConfigSource":"static","ConfigState":"configured"},{"Family":10,"Address":[40,3,96,128,137,4,146,34,0,0,0,0,0,0,0,1],"PrefixLength":64,"Scope":0,"ScopeString":"global","Flags":160,"FlagsString":"deprecated,permanent","PreferredLifetimeUsec":600951316,"ConfigSource":"static","ConfigState":"configured"}],"Routes":[{"Family":10,"Destination":[36,1,219,0,48,44,65,36,250,206,0,0,2,34,0,0],"DestinationPrefixLength":128,"Scope":0,"ScopeString":"global","Protocol":2,"ProtocolString":"kernel","Type":2,"TypeString":"local","Priority":0,"Table":255,"TableString":"local(255)","Preference":0,"Flags":0,"FlagsString":"","ConfigSource":"foreign","ConfigState":"configured"},{"Family":10,"Destination":[40,3,96,128,137,4,146,34,0,0,0,0,0,0,0,0],"DestinationPrefixLength":64,"Scope":0,"ScopeString":"global","Protocol":2,"ProtocolString":"kernel","Type":1,"TypeString":"unicast","Priority":256,"Table":254,"TableString":"main(254)","Preference":0,"Flags":0,"FlagsString":"","ConfigSource":"foreign","ConfigState":"configured"},{"Family":10,"Destination":[254,128,0,0,0,0,0,0,190,151,225,255,254,137,250,44],"DestinationPrefixLength":128,"Scope":0,"ScopeString":"global","Protocol":2,"ProtocolString":"kernel","Type":2,"TypeString":"local","Priority":0,"Table":255,"TableString":"local(255)","Preference":0,"Flags":0,"FlagsString":"","ConfigSource":"foreign","ConfigState":"configured"},{"Family":10,"Destination":[255,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"DestinationPrefixLength":8,"Scope":0,"ScopeString":"global","Protocol":2,"ProtocolString":"kernel","Type":5,"TypeString":"multicast","Priority":256,"Table":255,"TableString":"local(255)","Preference":0,"Flags":0,"FlagsString":"","ConfigSource":"foreign","ConfigState":"configured"},{"Family":10,"Destination":[40,3,96,128,137,4,146,34,0,0,0,0,0,0,0,1],"DestinationPrefixLength":128,"Scope":0,"ScopeString":"global","Protocol":2,"ProtocolString":"kernel","Type":2,"TypeString":"local","Priority":0,"Table":255,"TableString":"local(255)","Preference":0,"Flags":0,"FlagsString":"","ConfigSource":"foreign","ConfigState":"configured"},{"Family":10,"Destination":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"DestinationPrefixLength":0,"Gateway":[254,128,0,0,0,0,0,0,0,0,0,0,250,206,176,12],"Scope":0,"ScopeString":"global","Protocol":4,"ProtocolString":"static","Type":1,"TypeString":"unicast","Priority":10,"Table":254,"TableString":"main(254)","Preference":0,"Flags":0,"FlagsString":"","ConfigSource":"static","ConfigState":"configuring,configured"},{"Family":10,"Destination":[36,1,219,0,48,44,65,36,0,0,0,0,0,0,0,0],"DestinationPrefixLength":64,"Scope":0,"ScopeString":"global","Protocol":2,"ProtocolString":"kernel","Type":1,"TypeString":"unicast","Priority":256,"Table":254,"TableString":"main(254)","Preference":0,"Flags":0,"FlagsString":"","ConfigSource":"foreign","ConfigState":"configured"},{"Family":10,"Destination":[254,128,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"DestinationPrefixLength":64,"Scope":0,"ScopeString":"global","Protocol":2,"ProtocolString":"kernel","Type":1,"TypeString":"unicast","Priority":256,"Table":254,"TableString":"main(254)","Preference":0,"Flags":0,"FlagsString":"","ConfigSource":"foreign","ConfigState":"configured"}]}],"RoutingPolicyRules":[{"Family":2,"Protocol":2,"ProtocolString":"kernel","TOS":0,"Type":1,"TypeString":"table","IPProtocol":0,"IPProtocolString":"ip","Priority":32767,"FirewallMark":0,"FirewallMask":0,"Table":253,"TableString":"default(253)","Invert":false,"ConfigSource":"foreign","ConfigState":"configured"},{"Family":10,"Protocol":2,"ProtocolString":"kernel","TOS":0,"Type":1,"TypeString":"table","IPProtocol":0,"IPProtocolString":"ip","Priority":0,"FirewallMark":0,"FirewallMask":0,"Table":255,"TableString":"local(255)","Invert":false,"ConfigSource":"foreign","ConfigState":"configured"},{"Family":2,"Protocol":2,"ProtocolString":"kernel","TOS":0,"Type":1,"TypeString":"table","IPProtocol":0,"IPProtocolString":"ip","Priority":0,"FirewallMark":0,"FirewallMask":0,"Table":255,"TableString":"local(255)","Invert":false,"ConfigSource":"foreign","ConfigState":"configured"},{"Family":10,"Protocol":2,"ProtocolString":"kernel","TOS":0,"Type":1,"TypeString":"table","IPProtocol":0,"IPProtocolString":"ip","Priority":32766,"FirewallMark":0,"FirewallMask":0,"Table":254,"TableString":"main(254)","Invert":false,"ConfigSource":"foreign","ConfigState":"configured"},{"Family":2,"Protocol":2,"ProtocolString":"kernel","TOS":0,"Type":1,"TypeString":"table","IPProtocol":0,"IPProtocolString":"ip","Priority":32766,"FirewallMark":0,"FirewallMask":0,"Table":254,"TableString":"main(254)","Invert":false,"ConfigSource":"foreign","ConfigState":"configured"}]}"###;

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

    fn return_echo_args() -> Vec<String> {
        vec![NETWORKCTL_JSON.to_string()]
    }

    #[test]
    fn test_interface_id_to_name() {
        let networkctl_json_parsed =
            parse_networkctl_list(ECHO_BINARY, return_echo_args()).unwrap();
        assert_eq!(
            "eth0".to_string(),
            interface_id_to_name(2, &networkctl_json_parsed),
        );
    }

    #[test]
    fn test_parse_interface_stats() {
        let networkctl_json = parse_networkctl_list(ECHO_BINARY, return_echo_args()).unwrap();
        assert_eq!(
            return_expected_interface_state(),
            parse_interface_stats(MOCK_INTERFACE_STATE.to_string(), 2, Some(&networkctl_json))
                .unwrap(),
        );
    }

    #[test]
    fn test_parse_interface_stats_json() {
        // 'name' stays as an empty string cause we don't pass in networkctl json or an interface id
        let expected_interface_state_json = r###"{"address_state":3,"admin_state":4,"carrier_state":5,"ipv4_address_state":3,"ipv6_address_state":2,"name":"","network_file":"/etc/systemd/network/69-eno4.network","oper_state":9,"required_for_online":1}"###;
        let stats = parse_interface_stats(MOCK_INTERFACE_STATE.to_string(), 0, None).unwrap();
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
            parse_interface_state_files(path, ECHO_BINARY, return_echo_args()).unwrap()
        );
        Ok(())
    }

    #[test]
    fn test_parse_interface_state_files_json() -> Result<()> {
        let expected_interface_state_json = r###"{"interfaces_state":[{"address_state":3,"admin_state":4,"carrier_state":5,"ipv4_address_state":3,"ipv6_address_state":2,"name":"","network_file":"/etc/systemd/network/69-eno4.network","oper_state":9,"required_for_online":1}],"managed_interfaces":1}"###;

        let temp_dir = tempdir()?;
        // As the networkctl JSON has no interface with index 69 name gets no value ...
        let file_path = temp_dir.path().join("69");
        let mut state_file = File::create(file_path)?;
        writeln!(state_file, "{}", MOCK_INTERFACE_STATE)?;

        let path = PathBuf::from(temp_dir.path());
        let interface_stats =
            parse_interface_state_files(path, ECHO_BINARY, vec![NETWORKCTL_JSON.to_string()])
                .unwrap();
        let interface_stats_json = serde_json::to_string(&interface_stats).unwrap();
        assert_eq!(
            expected_interface_state_json.to_string(),
            interface_stats_json
        );
        Ok(())
    }

    #[test]
    /// Test to show that if we get valid JSON in stdout we're doing the right thing ...
    fn test_parse_networkctl_json() -> Result<()> {
        let expected_json_value: serde_json::Value = serde_json::from_str(NETWORKCTL_JSON).unwrap();
        assert_eq!(
            expected_json_value,
            // We rely on echo existing at this path - Could move to `sh -c echo` if actions has issues
            parse_networkctl_list(ECHO_BINARY, return_echo_args()).unwrap()
        );
        Ok(())
    }
}
