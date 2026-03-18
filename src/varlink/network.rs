//! Varlink proxy for the io.systemd.Network interface.
//! Adapted from the interface definition in systemd's
//! `src/shared/varlink-io.systemd.Network.c`.

use serde::{Deserialize, Serialize};
use zlink::{proxy, ReplyError};

pub const NETWORK_SOCKET_PATH: &str = "/run/systemd/netif/io.systemd.Network";

/// Proxy trait for calling methods on the io.systemd.Network interface.
#[proxy("io.systemd.Network")]
pub trait Network {
    /// Describe all interfaces managed by systemd-networkd.
    async fn describe(&mut self) -> zlink::Result<Result<DescribeOutput, NetworkError>>;
}

/// Output parameters for the Describe method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DescribeOutput {
    /// All network interfaces known to systemd-networkd.
    #[serde(rename = "Interfaces")]
    pub interfaces: Option<Vec<Interface>>,
}

/// Per-interface information returned by io.systemd.Network.Describe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interface {
    /// Kernel interface index.
    #[serde(rename = "Index")]
    pub index: i64,
    /// Primary interface name (e.g. "eth0").
    #[serde(rename = "Name")]
    pub name: String,
    /// Administrative state (configured, configuring, pending, …).
    #[serde(rename = "AdministrativeState")]
    pub administrative_state: String,
    /// Operational state (routable, degraded, carrier, …).
    #[serde(rename = "OperationalState")]
    pub operational_state: String,
    /// Carrier state (carrier, no-carrier, …).
    #[serde(rename = "CarrierState")]
    pub carrier_state: String,
    /// Combined address state across all address families.
    #[serde(rename = "AddressState")]
    pub address_state: String,
    /// IPv4-specific address state.
    #[serde(rename = "IPv4AddressState")]
    pub ipv4_address_state: String,
    /// IPv6-specific address state.
    #[serde(rename = "IPv6AddressState")]
    pub ipv6_address_state: String,
    /// Overall online state (online, offline, partial); absent when unknown.
    #[serde(rename = "OnlineState")]
    pub online_state: Option<String>,
    /// Path to the applied .network configuration file; absent for unmanaged interfaces.
    #[serde(rename = "NetworkFile")]
    pub network_file: Option<String>,
    /// Whether this interface is required for the system to be considered online.
    #[serde(rename = "RequiredForOnline")]
    pub required_for_online: Option<bool>,
}

/// Errors that can occur in the io.systemd.Network interface.
#[derive(Debug, Clone, PartialEq, ReplyError)]
#[zlink(interface = "io.systemd.Network")]
pub enum NetworkError {
    /// The requested interface was not found.
    NoSuchInterface,
}

impl std::fmt::Display for NetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkError::NoSuchInterface => write!(f, "No such interface"),
        }
    }
}

impl std::error::Error for NetworkError {}
