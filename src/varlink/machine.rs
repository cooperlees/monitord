//! Varlink proxy for systemd-machined's io.systemd.Machine interface.
//! Adapted from the interface definition in systemd's
//! `src/shared/varlink-io.systemd.Machine.c`. machined gained this API in
//! systemd v257.

use serde::{Deserialize, Serialize};
use zlink::{proxy, ReplyError};

use crate::varlink::unit::ProcessId;

pub const MACHINE_SOCKET_PATH: &str = "/run/systemd/machine/io.systemd.Machine";

/// Proxy trait for calling methods on the io.systemd.Machine interface.
#[proxy("io.systemd.Machine")]
pub trait Machine {
    /// List every registered machine, one reply per machine.
    /// [Requires 'more' flag]
    #[zlink(more)]
    async fn list(
        &mut self,
    ) -> zlink::Result<
        impl futures_util::Stream<Item = zlink::Result<Result<ListOutput, MachineError>>>,
    >;
}

/// Output parameters for the List method.
///
/// Only the fields monitord consumes are declared; serde drops the rest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListOutput {
    /// Name of the machine, e.g. "testbox" or ".host".
    pub name: String,
    /// The class of this machine: "container", "vm" or "host".
    pub class: String,
    /// Leader process of this machine.
    pub leader: Option<ProcessId>,
}

/// Errors that can occur in the io.systemd.Machine interface.
#[derive(Debug, Clone, PartialEq, ReplyError)]
#[zlink(interface = "io.systemd.Machine")]
pub enum MachineError {
    /// No machine matches the request (also what an empty List returns).
    NoSuchMachine,
    /// The request is not supported for this machine.
    NotSupported,
}

impl std::fmt::Display for MachineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MachineError::NoSuchMachine => write!(f, "No such machine"),
            MachineError::NotSupported => write!(f, "Not supported"),
        }
    }
}

impl std::error::Error for MachineError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_output_deserialize() {
        // A real reply from systemd 259's machined, trimmed to one machine.
        let json = r#"{"name":"testbox","id":"76c418060ef042fdb8b579e755988736","class":"container","service":"systemd-nspawn","unit":"systemd-nspawn@testbox.service","leader":{"pid":7430,"pidfdId":5367,"bootId":"6e68bb3ad68f43838ea71e746325e67b"},"timestamp":{"realtime":1789561088882217,"monotonic":86182609},"UID":0}"#;
        let machine: ListOutput = serde_json::from_str(json).unwrap();
        assert_eq!(machine.name, "testbox");
        assert_eq!(machine.class, "container");
        assert_eq!(machine.leader.and_then(|l| l.pid), Some(7430));
    }
}
