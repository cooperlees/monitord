//! Varlink proxy for the io.systemd.Manager interface on PID 1's socket.
//! Adapted from the interface definition in systemd's
//! `src/core/varlink-manager.c`.

use serde::{Deserialize, Serialize};
use zlink::{proxy, ReplyError};

pub const MANAGER_SOCKET_PATH: &str = "/run/systemd/io.systemd.Manager";

/// Proxy trait for calling methods on the io.systemd.Manager interface.
#[proxy("io.systemd.Manager")]
pub trait Manager {
    /// Describe the manager's configuration and runtime state.
    async fn describe(&mut self) -> zlink::Result<Result<DescribeOutput, ManagerError>>;
}

/// Output parameters for the Describe method.
///
/// Only the runtime fields monitord consumes are declared; serde drops the
/// rest of the (large) reply, including the whole `context` object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DescribeOutput {
    pub runtime: Option<ManagerRuntime>,
}

/// Runtime state of the systemd manager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagerRuntime {
    /// systemd version string, e.g. "262~rc3-1.fc46".
    #[serde(rename = "Version")]
    pub version: Option<String>,
    /// Overall system state, e.g. "running" or "degraded".
    #[serde(rename = "SystemState")]
    pub system_state: Option<String>,
}

/// Errors that can occur in the io.systemd.Manager interface.
#[derive(Debug, Clone, PartialEq, ReplyError)]
#[zlink(interface = "io.systemd.Manager")]
pub enum ManagerError {
    /// The manager is refusing calls because it is being hammered.
    RateLimitReached,
}

impl std::fmt::Display for ManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManagerError::RateLimitReached => write!(f, "Rate limit reached"),
        }
    }
}

impl std::error::Error for ManagerError {}
