//! Varlink proxy for the io.systemd.Unit interface on PID 1's socket.
//! Adapted from the interface definition in systemd's
//! `src/core/varlink-unit.c`. Available from systemd v258+.
//!
//! Only the fields monitord maps onto its own stats types are declared; serde
//! drops the rest, which is most of a ~7KB per-unit reply.
//!
//! Note that systemd omits fields sitting at their default rather than sending
//! a zero, so almost everything here is optional and the caller supplies the
//! default (see `varlink_units`, where those defaults have to match what the
//! D-Bus properties return for the same unit).

use serde::{Deserialize, Serialize};
use zlink::{proxy, ReplyError};

/// Proxy trait for calling methods on the io.systemd.Unit interface.
#[proxy("io.systemd.Unit")]
pub trait Unit {
    /// Look up a single unit by name.
    ///
    /// Called without the `more` flag and with a name, this returns one unit
    /// rather than streaming every unit — which is what monitord wants: a
    /// filtered call costs ~0.5ms against ~92ms to stream all 305 units on a
    /// test host, and monitord only needs a handful per collection.
    async fn list(&mut self, name: Option<&str>) -> zlink::Result<Result<ListOutput, UnitError>>;
}

/// Output parameters for the List method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListOutput {
    /// Unit configuration.
    pub context: Option<UnitContext>,
    /// Unit runtime state.
    pub runtime: Option<UnitRuntime>,
}

/// Configuration of a unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitContext {
    #[serde(rename = "Service")]
    pub service: Option<ServiceContext>,
}

/// Service-specific configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceContext {
    /// Service type: simple, oneshot, notify, notify-reload, …
    #[serde(rename = "Type")]
    pub r#type: Option<String>,
    /// Configured restart delay in microseconds.
    #[serde(rename = "RestartUSec")]
    pub restart_usec: Option<u64>,
    /// Timeout for cleaning up resources after the service exits.
    #[serde(rename = "TimeoutCleanUSec")]
    pub timeout_clean_usec: Option<u64>,
    /// Watchdog timeout in microseconds.
    #[serde(rename = "WatchdogUSec")]
    pub watchdog_usec: Option<u64>,
}

/// Runtime state of a unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitRuntime {
    #[serde(rename = "StateChangeTimestamp")]
    pub state_change_timestamp: Option<Timestamp>,
    #[serde(rename = "ActiveEnterTimestamp")]
    pub active_enter_timestamp: Option<Timestamp>,
    #[serde(rename = "InactiveExitTimestamp")]
    pub inactive_exit_timestamp: Option<Timestamp>,
    #[serde(rename = "CGroup")]
    pub cgroup: Option<CGroupRuntime>,
    #[serde(rename = "Service")]
    pub service: Option<ServiceRuntime>,
}

/// A systemd timestamp, carrying both clocks.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Timestamp {
    pub realtime: Option<u64>,
    pub monotonic: Option<u64>,
}

/// cgroup accounting for a unit. Fields appear only when the matching
/// accounting option is enabled for the unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CGroupRuntime {
    /// cgroup path, relative to the cgroup mount point.
    #[serde(rename = "Path")]
    pub path: Option<String>,
    #[serde(rename = "CPUUsageNSec")]
    pub cpu_usage_nsec: Option<u64>,
    #[serde(rename = "MemoryCurrent")]
    pub memory_current: Option<u64>,
    #[serde(rename = "MemoryAvailable")]
    pub memory_available: Option<u64>,
    #[serde(rename = "TasksCurrent")]
    pub tasks_current: Option<u64>,
    #[serde(rename = "IOReadBytes")]
    pub io_read_bytes: Option<u64>,
    #[serde(rename = "IOReadOperations")]
    pub io_read_operations: Option<u64>,
}

/// Service-specific runtime state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceRuntime {
    /// errno-style status reported by the service via sd_notify.
    #[serde(rename = "StatusErrno")]
    pub status_errno: Option<i32>,
    /// Number of times systemd has restarted this service.
    #[serde(rename = "NRestarts")]
    pub n_restarts: Option<u32>,
}

/// Errors that can occur in the io.systemd.Unit interface.
#[derive(Debug, Clone, PartialEq, ReplyError)]
#[zlink(interface = "io.systemd.Unit")]
pub enum UnitError {
    /// No unit by that name is loaded.
    NoSuchUnit,
}

impl std::fmt::Display for UnitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnitError::NoSuchUnit => write!(f, "No such unit"),
        }
    }
}

impl std::error::Error for UnitError {}
