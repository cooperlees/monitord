//! Where a varlink collector connects to.

use std::fmt;
use std::sync::Arc;

use crate::varlink::machine_connector::{MachineConnector, MachineSocket};

/// A varlink socket to connect to: either reachable by path from monitord's
/// own namespaces (the host), or inside a machine, connected from within the
/// machine's PID namespace by its [`MachineConnector`].
#[derive(Clone, Debug)]
pub enum VarlinkEndpoint {
    Path(String),
    Machine {
        connector: Arc<MachineConnector>,
        socket: MachineSocket,
    },
}

impl VarlinkEndpoint {
    pub async fn connect(&self) -> anyhow::Result<zlink::unix::Connection> {
        match self {
            Self::Path(path) => Ok(zlink::unix::connect(path).await?),
            Self::Machine { connector, socket } => connector.connect(*socket).await,
        }
    }
}

impl From<&str> for VarlinkEndpoint {
    fn from(path: &str) -> Self {
        Self::Path(path.to_string())
    }
}

impl From<String> for VarlinkEndpoint {
    fn from(path: String) -> Self {
        Self::Path(path)
    }
}

impl fmt::Display for VarlinkEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(path) => f.write_str(path),
            Self::Machine { connector, socket } => write!(
                f,
                "{} in machine with leader {}",
                socket.path(),
                connector.leader_pid()
            ),
        }
    }
}
